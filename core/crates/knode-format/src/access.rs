//! Settings, folders, agents, keys and grants (schema version 3).
//!
//! Groundwork: these read and write the tables and record every change. Knode Service decides
//! who may call them and applies them (capture routing, key checks); only a person may change
//! them, so every write here refuses any other provenance.

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use crate::space::{
    add_change, add_change_with_detail, check_name, next_ulid, provenance, provenance_id,
};
use crate::types::ActorKind;
use crate::{
    Agent, AgentKey, Error, Folder, FolderSpec, GrantLevel, Provenance, Result, Space, Timestamp,
    actions, setting_keys,
};

impl Space {
    /// A setting of the space, or `None` when it is not set. See
    /// [`setting_keys`](crate::setting_keys).
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    /// Every setting of the space, by key, `space.id` included.
    pub fn settings(&self) -> Result<std::collections::BTreeMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT key, value FROM settings ORDER BY key")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Sets a setting of the space, or removes it with `None`, and records the change. Returns
    /// whether anything changed. `space.id` never changes. Only a person can change settings.
    pub fn set_setting(&mut self, key: &str, value: Option<&str>, by: &Provenance) -> Result<bool> {
        person_only(by, "change the settings of a space")?;
        check_key(key)?;
        if key == setting_keys::SPACE_ID {
            return Err(Error::Invalid("the id of a space never changes".into()));
        }
        if self.setting(key)?.as_deref() == value {
            return Ok(false);
        }
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        match value {
            Some(value) => tx.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?,
            None => tx.execute("DELETE FROM settings WHERE key = ?1", [key])?,
        };
        let detail = serde_json::json!({ "value": value }).to_string();
        add_change_with_detail(&tx, now, prov, actions::SETTINGS_SET, key, None, &detail)?;
        tx.commit()?;
        Ok(true)
    }

    /// The folders that feed the space, oldest first.
    pub fn folders(&self) -> Result<Vec<Folder>> {
        query_folders(&self.conn, "ORDER BY f.created_at, f.id", [])
    }

    /// A folder by id, or `None`.
    pub fn folder(&self, id: &str) -> Result<Option<Folder>> {
        Ok(query_folders(&self.conn, "WHERE f.id = ?1", [id])?.pop())
    }

    /// Adds a folder, or changes the one with the given id (or with the same kind and path),
    /// records the change and returns the folder. Only a person can change folders.
    pub fn set_folder(&mut self, spec: &FolderSpec, by: &Provenance) -> Result<Folder> {
        person_only(by, "change the folders of a space")?;
        let kind = check_name("folder kind", spec.kind.trim())?;
        let path = spec.path.trim();
        if path.is_empty() {
            return Err(Error::Invalid("a folder needs a path".into()));
        }
        if !spec.rules.is_object() {
            return Err(Error::Invalid(
                "the rules of a folder are a JSON object".into(),
            ));
        }
        let rules = spec.rules.to_string();
        let capture = if spec.capture { "on" } else { "off" };
        let existing: Option<String> = match &spec.id {
            Some(id) => {
                if self.folder(id)?.is_none() {
                    return Err(Error::Invalid(format!("no folder has the id {id}")));
                }
                Some(id.clone())
            }
            None => self
                .conn
                .query_row(
                    "SELECT id FROM folders WHERE kind = ?1 AND path = ?2",
                    params![kind, path],
                    |row| row.get(0),
                )
                .optional()?,
        };
        let now = Timestamp::now();
        let id = match &existing {
            Some(id) => id.clone(),
            None => next_ulid(&mut self.ids).to_string(),
        };
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        if existing.is_some() {
            tx.execute(
                "UPDATE folders SET kind = ?2, path = ?3, capture = ?4, rules = ?5,
                                    updated_at = ?6, updated_by = ?7
                 WHERE id = ?1",
                params![id, kind, path, capture, rules, now.0, prov],
            )?;
        } else {
            tx.execute(
                "INSERT INTO folders (id, kind, path, capture, rules, created_at, created_by,
                                      updated_at, updated_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?6, ?7)",
                params![id, kind, path, capture, rules, now.0, prov],
            )?;
        }
        add_change(&tx, now, prov, actions::FOLDER_SET, &id, None)?;
        tx.commit()?;
        self.folder(&id)?
            .ok_or_else(|| Error::Invalid("the folder could not be read back".into()))
    }

    /// Removes a folder and records the change. Nothing captured or imported from it is
    /// removed. Returns whether there was one. Only a person can change folders.
    pub fn remove_folder(&mut self, id: &str, by: &Provenance) -> Result<bool> {
        person_only(by, "change the folders of a space")?;
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        if tx.execute("DELETE FROM folders WHERE id = ?1", [id])? == 0 {
            return Ok(false);
        }
        let prov = provenance_id(&tx, by)?;
        add_change(&tx, now, prov, actions::FOLDER_REMOVE, id, None)?;
        tx.commit()?;
        Ok(true)
    }

    /// The agents that may have access to the space, oldest first.
    pub fn agents(&self) -> Result<Vec<Agent>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT a.id, a.name, a.kind, a.created_at,
                    p.actor_kind, p.actor, p.session, p.source
             FROM agents a JOIN provenance p ON p.id = a.created_by
             ORDER BY a.created_at, a.id",
        )?;
        let rows = stmt.query_map([], agent_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Adds an agent and records the change, or returns it as it is when the space already has
    /// it. `id` is the agent's id in every space; `None` makes a new one. `name` is lower case
    /// letters, digits and `-`, at most 64 characters, and is unique in the space. Only a person
    /// can add agents.
    pub fn add_agent(
        &mut self,
        id: Option<&str>,
        name: &str,
        kind: &str,
        by: &Provenance,
    ) -> Result<Agent> {
        person_only(by, "add agents")?;
        let name = check_agent_name(name.trim())?;
        let kind = check_name("agent kind", kind.trim())?;
        let id = match id {
            Some(id) => ulid::Ulid::from_string(id.trim())
                .map_err(|_| Error::Invalid(format!("{id:?} is not an agent id (a ULID)")))?
                .to_string(),
            None => next_ulid(&mut self.ids).to_string(),
        };
        if let Some(agent) = self
            .agents()?
            .into_iter()
            .find(|a| a.id == id || a.name == name)
        {
            if agent.id == id && agent.name == name {
                return Ok(agent);
            }
            return Err(Error::Invalid(format!(
                "the space already has an agent named {:?} or with the id {id}",
                agent.name
            )));
        }
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        tx.execute(
            "INSERT INTO agents (id, name, kind, created_at, created_by)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, name, kind, now.0, prov],
        )?;
        add_change(&tx, now, prov, actions::AGENT_ADD, &id, None)?;
        tx.commit()?;
        self.agents()?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| Error::Invalid("the agent could not be read back".into()))
    }

    /// The fingerprints of an agent's keys, oldest first, revoked ones included.
    pub fn agent_keys(&self, agent_id: &str) -> Result<Vec<AgentKey>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT fingerprint, agent_id, created_at, revoked_at FROM agent_keys
             WHERE agent_id = ?1 ORDER BY created_at, fingerprint",
        )?;
        let rows = stmt.query_map([agent_id], |row| {
            Ok(AgentKey {
                fingerprint: row.get(0)?,
                agent_id: row.get(1)?,
                created_at: Timestamp(row.get(2)?),
                revoked_at: row.get::<_, Option<i64>>(3)?.map(Timestamp),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Adds the fingerprint of an agent's key (`sha256:` and 64 lower case hexadecimal digits),
    /// never the key, and records the change. Returns whether it was new. Only a person can
    /// issue keys.
    pub fn add_agent_key(
        &mut self,
        agent_id: &str,
        fingerprint: &str,
        by: &Provenance,
    ) -> Result<bool> {
        person_only(by, "issue keys")?;
        check_fingerprint(fingerprint)?;
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        require_agent(&tx, agent_id)?;
        let prov = provenance_id(&tx, by)?;
        let added = tx.execute(
            "INSERT OR IGNORE INTO agent_keys (fingerprint, agent_id, created_at, created_by)
             VALUES (?1, ?2, ?3, ?4)",
            params![fingerprint, agent_id, now.0, prov],
        )?;
        if added == 0 {
            return Ok(false);
        }
        add_change(&tx, now, prov, actions::AGENT_KEY_ADD, fingerprint, None)?;
        tx.commit()?;
        Ok(true)
    }

    /// Revokes a key by its fingerprint and records the change. A revoked key never becomes
    /// valid again. Returns whether a valid key was revoked. Only a person can revoke keys.
    pub fn revoke_agent_key(&mut self, fingerprint: &str, by: &Provenance) -> Result<bool> {
        person_only(by, "revoke keys")?;
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        let revoked = tx.execute(
            "UPDATE agent_keys SET revoked_at = ?2, revoked_by = ?3
             WHERE fingerprint = ?1 AND revoked_at IS NULL",
            params![fingerprint, now.0, prov],
        )?;
        if revoked == 0 {
            return Ok(false);
        }
        add_change(&tx, now, prov, actions::AGENT_KEY_REVOKE, fingerprint, None)?;
        tx.commit()?;
        Ok(true)
    }

    /// What an agent may do in the space: [`GrantLevel::None`] without a grant.
    pub fn grant(&self, agent_id: &str) -> Result<GrantLevel> {
        let level: Option<String> = self
            .conn
            .query_row(
                "SELECT level FROM grants WHERE agent_id = ?1",
                [agent_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(level.map_or(GrantLevel::None, |l| GrantLevel::parse(&l)))
    }

    /// Sets what an agent may do in the space and records the change with the level and the
    /// previous one. Returns whether it changed. Only a person can change grants.
    pub fn set_grant(
        &mut self,
        agent_id: &str,
        level: GrantLevel,
        by: &Provenance,
    ) -> Result<bool> {
        person_only(by, "change grants")?;
        let previous = self.grant(agent_id)?;
        let has_row: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM grants WHERE agent_id = ?1)",
            [agent_id],
            |row| row.get(0),
        )?;
        if previous == level && has_row {
            return Ok(false);
        }
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        require_agent(&tx, agent_id)?;
        let prov = provenance_id(&tx, by)?;
        tx.execute(
            "INSERT INTO grants (agent_id, level, updated_at, updated_by) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (agent_id) DO UPDATE SET
                 level = excluded.level, updated_at = excluded.updated_at,
                 updated_by = excluded.updated_by",
            params![agent_id, level.as_str(), now.0, prov],
        )?;
        let detail = serde_json::json!({ "level": level.as_str(), "previous": previous.as_str() })
            .to_string();
        add_change_with_detail(&tx, now, prov, actions::GRANT_SET, agent_id, None, &detail)?;
        tx.commit()?;
        Ok(true)
    }
}

fn person_only(by: &Provenance, what: &str) -> Result<()> {
    if by.kind == ActorKind::Person {
        Ok(())
    } else {
        Err(Error::PersonOnly(what.into()))
    }
}

/// Checks a settings key: `[a-z][a-z0-9_.-]*`, at most 64 characters.
fn check_key(key: &str) -> Result<()> {
    let ok = key.len() <= 64
        && key.starts_with(|c: char| c.is_ascii_lowercase())
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'));
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "a setting key is lower case letters, digits, _, . and -, starting with a letter: \
             {key:?}"
        )))
    }
}

/// Checks an agent name: `[a-z][a-z0-9-]*`, at most 64 characters.
fn check_agent_name(name: &str) -> Result<String> {
    let ok = name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(name.to_string())
    } else {
        Err(Error::Invalid(format!(
            "an agent name is lower case letters, digits and -, starting with a letter: {name:?}"
        )))
    }
}

/// Checks a key fingerprint: `sha256:` and 64 lower case hexadecimal digits.
fn check_fingerprint(fingerprint: &str) -> Result<()> {
    let ok = fingerprint.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    });
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "a key fingerprint is sha256: and 64 lower case hexadecimal digits: {fingerprint:?}"
        )))
    }
}

fn require_agent(tx: &Transaction<'_>, agent_id: &str) -> Result<()> {
    let found = tx
        .query_row("SELECT 1 FROM agents WHERE id = ?1", [agent_id], |_| Ok(()))
        .optional()?;
    found.ok_or_else(|| Error::Invalid(format!("no agent has the id {agent_id}")))
}

fn agent_row(row: &Row<'_>) -> rusqlite::Result<Agent> {
    Ok(Agent {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: row.get(2)?,
        created_at: Timestamp(row.get(3)?),
        created_by: provenance(row, 4)?,
    })
}

fn query_folders<P: rusqlite::Params>(
    conn: &Connection,
    tail: &str,
    params: P,
) -> Result<Vec<Folder>> {
    let sql = format!(
        "SELECT f.id, f.kind, f.path, f.capture, f.rules, f.created_at, f.updated_at,
                c.actor_kind, c.actor, c.session, c.source,
                u.actor_kind, u.actor, u.session, u.source
         FROM folders f
         JOIN provenance c ON c.id = f.created_by
         JOIN provenance u ON u.id = f.updated_by
         {tail}"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params, |row| {
        let capture: String = row.get(3)?;
        let rules: String = row.get(4)?;
        Ok(Folder {
            id: row.get(0)?,
            kind: row.get(1)?,
            path: row.get(2)?,
            capture: capture == "on",
            rules: serde_json::from_str(&rules)
                .unwrap_or_else(|_| serde_json::Value::Object(Default::default())),
            created_at: Timestamp(row.get(5)?),
            updated_at: Timestamp(row.get(6)?),
            created_by: provenance(row, 7)?,
            updated_by: provenance(row, 11)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
