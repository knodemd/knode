//! Who a connection acts for, and what it may do: agent keys, grants and the access log.
//!
//! Every connection starts anonymous and authenticates once with `auth`: with the owner token
//! of the Knode home it acts for the person, who is not limited by grants; with an agent key it
//! acts as that agent. For an agent, every call is checked, not just the connection: its key
//! must still be valid on this machine and in the space, and its level in the space must allow
//! the call. A space the agent may not see is invisible: naming it fails exactly as for a space
//! that does not exist. Section 16 of the .knode specification has the rules.

mod log;
mod registry;

use std::path::{Path, PathBuf};

use knode_format::{ActorKind, GrantLevel, Provenance, Space};
use serde_json::Value;

use crate::agents::{
    AgentAddParams, AgentInfo, AgentRevokeParams, AuthParams, GrantSetParams, Identity, KeyInfo,
    SpaceGrant,
};
use crate::keys::{FINGERPRINT_PREFIX, fingerprint, is_key, owner_token};
use crate::protocol::{RpcError, Status, codes, methods};
use crate::server::{State, call, lock_ignoring_poison, params, require_absolute, to_value};

pub use log::ACCESS_LOG_FILE;
use log::{AccessLog, Outcome};
pub use registry::AGENTS_FILE;
use registry::{Lookup, MachineAgent, Registry};

/// What the service knows to authenticate connections: the owner token, the agents of this
/// machine and the access log.
#[derive(Debug)]
pub(crate) struct Auth {
    owner: String,
    registry: std::sync::Mutex<Registry>,
    log: AccessLog,
}

impl Auth {
    /// Loads the agents of the Knode home `home` and makes its owner token the first time.
    pub fn load(home: &Path) -> std::io::Result<Auth> {
        Ok(Auth {
            owner: owner_token(home)?,
            registry: std::sync::Mutex::new(Registry::load(home)?),
            log: AccessLog::new(home),
        })
    }
}

/// An agent connected with a key.
#[derive(Debug, Clone)]
pub(crate) struct AgentSession {
    id: String,
    name: String,
    fingerprint: String,
}

/// Who a connection acts for.
#[derive(Debug, Clone, Default)]
pub(crate) enum Session {
    /// Not authenticated yet: only `auth` is answered.
    #[default]
    Anonymous,
    /// The person who owns the Knode home.
    Person,
    /// An agent.
    Agent(AgentSession),
}

impl Session {
    pub fn agent(&self) -> Option<&AgentSession> {
        match self {
            Session::Agent(agent) => Some(agent),
            _ => None,
        }
    }
}

fn unauthenticated(message: impl Into<String>) -> RpcError {
    RpcError::new(codes::UNAUTHENTICATED, message).with_kind("unauthenticated")
}

fn forbidden(message: impl Into<String>) -> RpcError {
    RpcError::new(codes::FORBIDDEN, message).with_kind("forbidden")
}

/// The error for a connection that has not authenticated.
pub(crate) fn anonymous() -> RpcError {
    unauthenticated(
        "this connection has not authenticated: an agent connects with its key, which \
         `knode connect <agent>` gives it",
    )
}

/// The same error as for a space that does not exist, for a space the agent may not see.
fn invisible(path: &Path) -> RpcError {
    RpcError::from(knode_format::Error::NotASpace(path.to_path_buf()))
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// `auth`: authenticates the connection once, as the person or as an agent.
pub(crate) fn handshake(
    state: &State,
    session: &mut Session,
    raw: Value,
) -> Result<Value, RpcError> {
    if !matches!(session, Session::Anonymous) {
        return Err(RpcError::invalid_params(
            "this connection has already authenticated",
        ));
    }
    let p: AuthParams = params(methods::AUTH, raw)?;
    let auth = &state.auth;
    let identity = match (p.owner, p.key) {
        (Some(owner), None) => {
            if !same(&owner, &auth.owner) {
                return Err(unauthenticated(
                    "this is not the owner token of this Knode home",
                ));
            }
            *session = Session::Person;
            Identity::Person
        }
        (None, Some(key)) => {
            let refuse = |agent: Option<&str>, message: String| {
                auth.log.record(agent, None, methods::AUTH, Outcome::Denied);
                Err(unauthenticated(message))
            };
            if !is_key(key.trim()) {
                return refuse(
                    None,
                    "this is not a Knode agent key; ask the person to run `knode connect <agent>`"
                        .into(),
                );
            }
            let fingerprint = fingerprint(key.trim());
            let registry = lock_ignoring_poison(&auth.registry);
            match registry.lookup(&fingerprint) {
                Lookup::Valid(agent) => {
                    *session = Session::Agent(AgentSession {
                        id: agent.id.clone(),
                        name: agent.name.clone(),
                        fingerprint,
                    });
                    auth.log
                        .record(Some(&agent.id), None, methods::AUTH, Outcome::Allowed);
                    Identity::Agent {
                        id: agent.id.clone(),
                        name: agent.name.clone(),
                        kind: agent.kind.clone(),
                    }
                }
                Lookup::Revoked(agent) => {
                    return refuse(
                        Some(&agent.id),
                        format!(
                            "this key of {} was revoked; ask the person to run `knode connect \
                             {}` to give it a new one",
                            agent.name, agent.kind
                        ),
                    );
                }
                Lookup::Unknown => {
                    return refuse(
                        None,
                        "Knode does not know this key; ask the person to run `knode connect \
                         <agent>` on this machine"
                            .into(),
                    );
                }
            }
        }
        _ => {
            return Err(RpcError::invalid_params(
                "auth takes either `key` (an agent key) or `owner` (the owner token)",
            ));
        }
    };
    to_value(identity)
}

/// What a method needs from an agent.
enum Access {
    Read,
    Write,
    Status,
    PersonOnly,
}

fn access(method: &str) -> Access {
    match method {
        methods::SPACE_OPEN
        | methods::NODE_GET
        | methods::NODE_FIND_BY_TITLE
        | methods::LINKS_FROM
        | methods::LINKS_BACK
        | methods::LINKS_UNRESOLVED
        | methods::SEARCH
        | methods::RECENT
        | methods::HISTORY
        | methods::CHANGES
        | methods::CHANGE
        | methods::VAULT_EXPORT
        | methods::MARKDOWN_FOLDER_GET => Access::Read,
        methods::NODE_CREATE
        | methods::NODE_UPDATE
        | methods::NODE_DELETE
        | methods::LINK_CREATE
        | methods::LINK_DELETE
        | methods::UNDO
        | methods::RESTORE
        | methods::VAULT_IMPORT => Access::Write,
        methods::SERVICE_STATUS => Access::Status,
        // Creating spaces, managing agents, purge, the live Markdown folder and anything not
        // listed here are for people only.
        _ => Access::PersonOnly,
    }
}

/// Runs one method for an authenticated connection. Blocking: called on a blocking thread.
pub(crate) fn dispatch(
    state: &State,
    session: &Session,
    method: &str,
    raw: Value,
) -> Result<Value, RpcError> {
    match session {
        Session::Anonymous => Err(anonymous()),
        Session::Person => match method {
            methods::AGENTS_LIST => to_value(list(state)),
            methods::AGENTS_ADD => add(state, params(method, raw)?),
            methods::AGENTS_REVOKE => revoke(state, params(method, raw)?),
            methods::GRANTS_SET => set_grant(state, params(method, raw)?),
            _ => call(state, method, raw),
        },
        Session::Agent(agent) => agent_call(state, agent, method, raw),
    }
}

/// An agent may only undo a change it made itself, and never with `force`: overwriting later
/// changes, or anyone else's, is for the person to decide.
fn own_undo(state: &State, agent: &AgentSession, path: &Path, raw: &Value) -> Result<(), RpcError> {
    if raw.get("force").and_then(Value::as_bool) == Some(true) {
        return Err(forbidden(
            "agents cannot force an undo; ask the person to run `knode undo <change> --force`",
        ));
    }
    let Some(change) = raw.get("change").and_then(Value::as_i64) else {
        return Ok(()); // The params are checked, and refused, by the call itself.
    };
    let space = state.space(path)?;
    let space = lock_ignoring_poison(&space);
    let Some(found) = space.change(change)? else {
        return Ok(()); // Answered as not found by the call.
    };
    let by = &found.change.changed_by;
    if by.kind == ActorKind::Agent && by.actor == agent.name {
        Ok(())
    } else {
        Err(forbidden(format!(
            "change {change} was made by {} ({}), not by {}; agents only undo their own              changes. Ask the person, who can run `knode undo {change}`",
            by.actor,
            by.kind.as_str(),
            agent.name
        )))
    }
}

/// Checks that the agent's key is still valid on this machine.
fn still_valid(state: &State, agent: &AgentSession) -> Result<(), RpcError> {
    let registry = lock_ignoring_poison(&state.auth.registry);
    match registry.lookup(&agent.fingerprint) {
        Lookup::Valid(found) if found.id == agent.id => Ok(()),
        _ => Err(unauthenticated(format!(
            "the key of {} was revoked; ask the person to run `knode connect` again",
            agent.name
        ))),
    }
}

/// The agent's level in the space at `path`, and the space's id; `None` for both when the
/// space cannot be opened.
fn level_in(state: &State, agent: &AgentSession, path: &Path) -> (GrantLevel, Option<String>) {
    let Ok(space) = state.space(path) else {
        return (GrantLevel::None, None);
    };
    let space = lock_ignoring_poison(&space);
    let id = space.manifest().id.clone();
    (
        level_of(&space, agent).unwrap_or(GrantLevel::None),
        Some(id),
    )
}

fn level_of(space: &Space, agent: &AgentSession) -> knode_format::Result<GrantLevel> {
    let valid = space
        .agent_keys(&agent.id)?
        .iter()
        .any(|k| k.fingerprint == agent.fingerprint && k.revoked_at.is_none());
    if !valid {
        return Ok(GrantLevel::None);
    }
    space.grant(&agent.id)
}

/// Whether the agent may see the space the service opened at `path`.
pub(crate) fn can_read(state: &State, agent: &AgentSession, path: &Path) -> bool {
    still_valid(state, agent).is_ok() && level_in(state, agent, path).0 >= GrantLevel::Read
}

/// Checks `subscribe` to one space for an agent: the space must be visible to it.
pub(crate) fn check_subscribe(
    state: &State,
    agent: &AgentSession,
    space: &Path,
) -> Result<(), RpcError> {
    still_valid(state, agent)?;
    require_absolute(space)?;
    let (level, id) = level_in(state, agent, space);
    if level < GrantLevel::Read {
        state.auth.log.record(
            Some(&agent.id),
            id.as_deref(),
            methods::SUBSCRIBE,
            Outcome::Invisible,
        );
        return Err(invisible(space));
    }
    state.auth.log.record(
        Some(&agent.id),
        id.as_deref(),
        methods::SUBSCRIBE,
        Outcome::Allowed,
    );
    Ok(())
}

fn agent_call(
    state: &State,
    agent: &AgentSession,
    method: &str,
    mut raw: Value,
) -> Result<Value, RpcError> {
    still_valid(state, agent)?;
    let log = &state.auth.log;
    let needs = match access(method) {
        Access::Status => return to_value(agent_status(state, agent)),
        Access::PersonOnly => {
            log.record(Some(&agent.id), None, method, Outcome::Denied);
            return Err(forbidden(format!(
                "only a person can call {method}; agents read and write the spaces they were \
                 given"
            )));
        }
        Access::Read => GrantLevel::Read,
        Access::Write => GrantLevel::Write,
    };
    let Some(path) = raw.get("space").and_then(Value::as_str).map(PathBuf::from) else {
        return Err(RpcError::invalid_params(format!(
            "invalid params for {method}: missing field `space`"
        )));
    };
    require_absolute(&path)?;
    let (level, id) = level_in(state, agent, &path);
    let id = id.as_deref();
    if level == GrantLevel::None {
        log.record(Some(&agent.id), id, method, Outcome::Invisible);
        return Err(invisible(&path));
    }
    if level < needs {
        log.record(Some(&agent.id), id, method, Outcome::Denied);
        return Err(forbidden(format!(
            "{} may only read this space, not change it",
            agent.name
        )));
    }
    if method == methods::UNDO {
        if let Err(error) = own_undo(state, agent, &path, &raw) {
            log.record(Some(&agent.id), id, method, Outcome::Denied);
            return Err(error);
        }
    }
    // An agent's changes carry the name its key belongs to, never one it gives itself.
    if method == methods::LINK_DELETE && raw.get("by").is_none_or(Value::is_null) {
        if let Some(raw) = raw.as_object_mut() {
            raw.insert(
                "by".into(),
                serde_json::json!({ "kind": "agent", "actor": "" }),
            );
        }
    }
    if let Some(by) = raw.get_mut("by").and_then(Value::as_object_mut) {
        by.insert("kind".into(), Value::String("agent".into()));
        by.insert("actor".into(), Value::String(agent.name.clone()));
    }
    log.record(Some(&agent.id), id, method, Outcome::Allowed);
    call(state, method, raw)
}

/// `service.status` as an agent sees it: only the spaces it may see.
fn agent_status(state: &State, agent: &AgentSession) -> Status {
    let mut status = state.status();
    status
        .spaces
        .retain(|space| level_in(state, agent, space).0 >= GrantLevel::Read);
    if let Some(capture) = &status.capture
        && !status.spaces.contains(capture)
    {
        status.capture = None;
    }
    status
}

// Managing agents: people only.

fn person(by: &Provenance) -> Result<(), RpcError> {
    if by.kind == ActorKind::Person {
        Ok(())
    } else {
        Err(forbidden(
            "only a person can manage agents, keys and grants",
        ))
    }
}

fn check_agent_name(what: &str, name: &str) -> Result<(), RpcError> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(RpcError::invalid_params(format!(
            "an agent {what} is lower case letters, digits and -, starting with a letter: {name:?}"
        )))
    }
}

fn check_fingerprint(fingerprint: &str) -> Result<(), RpcError> {
    let ok = fingerprint
        .strip_prefix(FINGERPRINT_PREFIX)
        .is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        });
    if ok {
        Ok(())
    } else {
        Err(RpcError::invalid_params(format!(
            "a key fingerprint is sha256: and 64 lower case hexadecimal digits: {fingerprint:?}"
        )))
    }
}

fn info(state: &State, agent: &MachineAgent) -> AgentInfo {
    let spaces = agent
        .spaces
        .iter()
        .map(|path| {
            let level = state.space(path).ok().map(|space| {
                let space = lock_ignoring_poison(&space);
                if space
                    .agents()
                    .is_ok_and(|a| a.iter().any(|a| a.id == agent.id))
                {
                    space.grant(&agent.id).unwrap_or(GrantLevel::None)
                } else {
                    GrantLevel::None
                }
            });
            SpaceGrant {
                space: path.clone(),
                level,
            }
        })
        .collect();
    AgentInfo {
        id: agent.id.clone(),
        name: agent.name.clone(),
        kind: agent.kind.clone(),
        created_at: agent.created_at,
        keys: agent.keys.clone(),
        spaces,
    }
}

/// `agents.list`: the agents of this machine.
fn list(state: &State) -> Vec<AgentInfo> {
    let agents = lock_ignoring_poison(&state.auth.registry).agents.clone();
    agents.iter().map(|agent| info(state, agent)).collect()
}

fn unknown_agent(name: &str) -> RpcError {
    RpcError::invalid_params(format!(
        "there is no agent named {name:?} on this machine; connect it with `knode connect`"
    ))
}

/// Writes the agent and the fingerprints of all its keys, revocations included, into a space.
fn sync_into(space: &mut Space, agent: &MachineAgent, by: &Provenance) -> Result<(), RpcError> {
    space.add_agent(Some(&agent.id), &agent.name, &agent.kind, by)?;
    let known = space.agent_keys(&agent.id)?;
    for key in &agent.keys {
        if !known.iter().any(|k| k.fingerprint == key.fingerprint) {
            space.add_agent_key(&agent.id, &key.fingerprint, by)?;
        }
        if key.revoked_at.is_some() {
            space.revoke_agent_key(&key.fingerprint, by)?;
        }
    }
    Ok(())
}

/// Writes the agent's keys into every space it was given a level in. A space that cannot be
/// opened now is left out: the key is refused on this machine anyway.
fn sync_everywhere(state: &State, agent: &MachineAgent, by: &Provenance, revoke: bool) {
    for path in &agent.spaces {
        let Ok(space) = state.space(path) else {
            continue;
        };
        let mut space = lock_ignoring_poison(&space);
        if let Err(e) = sync_into(&mut space, agent, by) {
            eprintln!(
                "Knode Service: cannot update the keys of {} in {}: {e}",
                agent.name,
                path.display()
            );
            continue;
        }
        if revoke {
            let _ = space.set_grant(&agent.id, GrantLevel::None, by);
        }
    }
}

/// `agents.add`: adds an agent with the fingerprint of its new key, or gives an agent a new key
/// and revokes its old ones.
fn add(state: &State, p: AgentAddParams) -> Result<Value, RpcError> {
    person(&p.by)?;
    check_agent_name("name", &p.name)?;
    check_agent_name("kind", &p.kind)?;
    check_fingerprint(&p.fingerprint)?;
    let now = knode_format::Timestamp::now().0;
    let agent = {
        let mut registry = lock_ignoring_poison(&state.auth.registry);
        if let Lookup::Valid(other) | Lookup::Revoked(other) = registry.lookup(&p.fingerprint)
            && other.name != p.name
        {
            return Err(RpcError::invalid_params(
                "this key belongs to another agent",
            ));
        }
        let agent = match registry.by_name_mut(&p.name) {
            Some(agent) => {
                if !agent.keys.iter().any(|k| k.fingerprint == p.fingerprint) {
                    for key in agent.keys.iter_mut().filter(|k| k.revoked_at.is_none()) {
                        key.revoked_at = Some(now);
                    }
                    agent.keys.push(KeyInfo {
                        fingerprint: p.fingerprint.clone(),
                        created_at: now,
                        revoked_at: None,
                    });
                }
                agent.clone()
            }
            None => {
                let agent = MachineAgent {
                    id: ulid::Ulid::generate().to_string(),
                    name: p.name.clone(),
                    kind: p.kind.clone(),
                    created_at: now,
                    keys: vec![KeyInfo {
                        fingerprint: p.fingerprint.clone(),
                        created_at: now,
                        revoked_at: None,
                    }],
                    spaces: Vec::new(),
                };
                registry.agents.push(agent.clone());
                agent
            }
        };
        registry
            .save()
            .map_err(|e| RpcError::new(codes::INTERNAL_ERROR, e.to_string()))?;
        agent
    };
    sync_everywhere(state, &agent, &p.by, false);
    to_value(info(state, &agent))
}

/// `agents.revoke`: revokes every key of an agent and takes away its access to every space it
/// was given. The agent keeps its name, so its past changes still have one.
fn revoke(state: &State, p: AgentRevokeParams) -> Result<Value, RpcError> {
    person(&p.by)?;
    let now = knode_format::Timestamp::now().0;
    let agent = {
        let mut registry = lock_ignoring_poison(&state.auth.registry);
        let agent = registry
            .by_name_mut(&p.agent)
            .ok_or_else(|| unknown_agent(&p.agent))?;
        for key in agent.keys.iter_mut().filter(|k| k.revoked_at.is_none()) {
            key.revoked_at = Some(now);
        }
        let agent = agent.clone();
        registry
            .save()
            .map_err(|e| RpcError::new(codes::INTERNAL_ERROR, e.to_string()))?;
        agent
    };
    sync_everywhere(state, &agent, &p.by, true);
    to_value(info(state, &agent))
}

/// `grants.set`: sets what an agent may do in a space, writing the agent and its key
/// fingerprints into the space first.
fn set_grant(state: &State, p: GrantSetParams) -> Result<Value, RpcError> {
    person(&p.by)?;
    let agent = lock_ignoring_poison(&state.auth.registry)
        .by_name(&p.agent)
        .cloned()
        .ok_or_else(|| unknown_agent(&p.agent))?;
    let space = state.space(&p.space)?;
    let path = {
        let mut space = lock_ignoring_poison(&space);
        sync_into(&mut space, &agent, &p.by)?;
        space.set_grant(&agent.id, p.level, &p.by)?;
        space.path().to_path_buf()
    };
    {
        let mut registry = lock_ignoring_poison(&state.auth.registry);
        if let Some(agent) = registry.by_name_mut(&p.agent)
            && !agent.spaces.contains(&path)
        {
            agent.spaces.push(path.clone());
            registry
                .save()
                .map_err(|e| RpcError::new(codes::INTERNAL_ERROR, e.to_string()))?;
        }
    }
    to_value(SpaceGrant {
        space: path,
        level: Some(p.level),
    })
}
