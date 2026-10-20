//! Agent keys and grants: a real service, the person and agents as clients.

use std::path::PathBuf;
use std::time::Duration;

use knode_format::{GrantLevel, NewNode, Provenance, Space};
use knode_service::keys::{fingerprint, generate_key};
use knode_service::protocol::codes;
use knode_service::{
    ACCESS_LOG_FILE, Client, Config, Error, Event, Identity, ServiceHandle, start,
};
use serde_json::json;
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
    _service: ServiceHandle,
    person: Client,
}

fn me() -> Provenance {
    Provenance::person("Ada")
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let service = start(Config::new(dir.path().join("home"))).unwrap();
        let person = Client::connect(service.socket_path()).unwrap();
        assert_eq!(person.identity(), Some(&Identity::Person));
        let fixture = Fixture {
            dir,
            _service: service,
            person,
        };
        for name in ["Work", "Personal"] {
            fixture
                .person
                .create_space(fixture.space(name), name)
                .unwrap();
        }
        fixture
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join("home/service.sock")
    }

    fn space(&self, name: &str) -> PathBuf {
        std::fs::canonicalize(self.dir.path())
            .unwrap()
            .join(format!("{name}.knode"))
    }

    /// Adds an agent and returns its key.
    fn agent(&self, name: &str) -> String {
        let key = generate_key().unwrap();
        self.person
            .add_agent(name, "claude-code", &fingerprint(&key), &me())
            .unwrap();
        key
    }

    fn connect(&self, key: &str) -> Client {
        Client::connect_as_agent(self.socket(), key).unwrap()
    }

    fn grant(&self, agent: &str, space: &str, level: GrantLevel) {
        self.person
            .set_grant(agent, self.space(space), level, &me())
            .unwrap();
    }
}

fn rpc(result: Result<impl std::fmt::Debug, Error>) -> knode_service::RpcError {
    match result {
        Err(Error::Rpc(e)) => e,
        other => panic!("expected an error from the service, got {other:?}"),
    }
}

#[test]
fn a_work_agent_cannot_see_a_personal_space() {
    let f = Fixture::new();
    let personal = f.space("Personal");
    f.person
        .create_node(&personal, NewNode::note("Diary", "Private."), &me())
        .unwrap();
    let key = f.agent("claude-code");
    f.grant("claude-code", "Work", GrantLevel::Write);
    let agent = f.connect(&key);
    match agent.identity() {
        Some(Identity::Agent { name, .. }) => assert_eq!(name, "claude-code"),
        other => panic!("{other:?}"),
    }

    // Write: allowed, recorded under the name its key belongs to, whatever it says it is.
    let node = agent
        .create_node(
            f.space("Work"),
            NewNode::note("Plan", "Ship."),
            &Provenance::agent("someone-else").with_session("s1"),
        )
        .unwrap();
    assert_eq!(node.created_by.actor, "claude-code");
    assert_eq!(node.created_by.session.as_deref(), Some("s1"));
    assert_eq!(agent.search(f.space("Work"), "ship", 10).unwrap().len(), 1);

    // None: the personal space fails exactly like a space that is not there.
    let hidden = rpc(agent.search(&personal, "private", 10));
    let missing_path = f.space("Missing");
    let missing = rpc(agent.search(&missing_path, "private", 10));
    assert_eq!(hidden.code, missing.code);
    assert_eq!(hidden.data, missing.data);
    assert_eq!(
        hidden.message.replace(personal.to_str().unwrap(), "X"),
        missing.message.replace(missing_path.to_str().unwrap(), "X")
    );
    assert_eq!(hidden.kind(), Some("not_a_space"));
    let hidden = rpc(agent.create_node(&personal, NewNode::note("x", ""), &me()));
    assert_eq!(hidden.kind(), Some("not_a_space"));
    let hidden = rpc(agent.call("subscribe", json!({ "space": personal })));
    assert_eq!(hidden.kind(), Some("not_a_space"));

    // service.status lists only what it may see.
    let status = agent.status().unwrap();
    assert_eq!(status.spaces, vec![f.space("Work")]);
    assert_eq!(f.person.status().unwrap().spaces.len(), 2);

    // Notifications: only for spaces it may see.
    let changes = agent.subscribe(None).unwrap();
    f.person
        .create_node(&personal, NewNode::note("Secret", ""), &me())
        .unwrap();
    let work = f
        .person
        .create_node(f.space("Work"), NewNode::note("Visible", ""), &me())
        .unwrap();
    match changes.recv_timeout(Duration::from_secs(5)) {
        Some(Event::Changed(change)) => assert_eq!(change.node_id, work.id),
        other => panic!("{other:?}"),
    }
    assert!(changes.recv_timeout(Duration::from_millis(300)).is_none());

    // The space holds the agent, its fingerprint and its grant; never the key.
    let space = Space::open(f.space("Work")).unwrap();
    let agents = space.agents().unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(
        space.agent_keys(&agents[0].id).unwrap()[0].fingerprint,
        fingerprint(&key)
    );
    assert_eq!(space.grant(&agents[0].id).unwrap(), GrantLevel::Write);
    assert!(Space::open(&personal).unwrap().agents().unwrap().is_empty());
}

#[test]
fn read_allows_reading_and_refuses_writing() {
    let f = Fixture::new();
    let key = f.agent("claude-code");
    f.grant("claude-code", "Work", GrantLevel::Read);
    let note = f
        .person
        .create_node(f.space("Work"), NewNode::note("Plan", "Ship."), &me())
        .unwrap();
    let agent = f.connect(&key);
    assert!(agent.get_node(f.space("Work"), &note.id).unwrap().is_some());
    assert_eq!(agent.recent(f.space("Work"), 5).unwrap().len(), 1);
    assert_eq!(agent.history(f.space("Work"), &note.id).unwrap().len(), 1);
    let refused = rpc(agent.create_node(f.space("Work"), NewNode::note("x", ""), &me()));
    assert_eq!(refused.code, codes::FORBIDDEN);
    assert_eq!(refused.kind(), Some("forbidden"));
    assert!(refused.message.contains("only read"), "{}", refused.message);

    // Raising and lowering take effect on the next call of the same connection.
    f.grant("claude-code", "Work", GrantLevel::Write);
    agent
        .create_node(f.space("Work"), NewNode::note("Now", ""), &me())
        .unwrap();
    f.grant("claude-code", "Work", GrantLevel::None);
    let hidden = rpc(agent.recent(f.space("Work"), 5));
    assert_eq!(hidden.kind(), Some("not_a_space"));
}

#[test]
fn only_people_manage_access() {
    let f = Fixture::new();
    let key = f.agent("claude-code");
    f.grant("claude-code", "Work", GrantLevel::Write);
    let agent = f.connect(&key);
    for (method, params) in [
        ("agents.list", json!({})),
        (
            "grants.set",
            json!({ "agent": "claude-code", "space": f.space("Personal"), "level": "write", "by": { "kind": "person", "actor": "Ada" } }),
        ),
        (
            "agents.revoke",
            json!({ "agent": "claude-code", "by": { "kind": "person", "actor": "Ada" } }),
        ),
        (
            "space.create",
            json!({ "space": f.space("New"), "name": "New" }),
        ),
        ("some.future.method", json!({ "space": f.space("Work") })),
    ] {
        let refused = rpc(agent.call(method, params));
        assert_eq!(refused.code, codes::FORBIDDEN, "{method}");
    }
    // A person's client cannot pass an agent off as a person either.
    let refused = rpc(f.person.set_grant(
        "claude-code",
        f.space("Personal"),
        GrantLevel::Write,
        &Provenance::agent("claude-code"),
    ));
    assert_eq!(refused.code, codes::FORBIDDEN);
}

#[test]
fn unknown_and_revoked_keys_are_refused() {
    let f = Fixture::new();
    let unknown = rpc(Client::connect_as_agent(
        f.socket(),
        &generate_key().unwrap(),
    ));
    assert_eq!(unknown.code, codes::UNAUTHENTICATED);
    assert!(
        unknown.message.contains("knode connect"),
        "{}",
        unknown.message
    );
    let bad = rpc(Client::connect_as_agent(f.socket(), "not a key"));
    assert_eq!(bad.code, codes::UNAUTHENTICATED);

    // A connection without auth is refused.
    let anonymous = Client::connect_unauthenticated(f.socket()).unwrap();
    let refused = rpc(anonymous.recent(f.space("Work"), 5));
    assert_eq!(refused.code, codes::UNAUTHENTICATED);
    assert!(
        refused.message.contains("knode connect"),
        "{}",
        refused.message
    );

    let key = f.agent("claude-code");
    f.grant("claude-code", "Work", GrantLevel::Write);
    let agent = f.connect(&key);
    agent.recent(f.space("Work"), 5).unwrap();
    let revoked = f.person.revoke_agent("claude-code", &me()).unwrap();
    assert!(!revoked.has_valid_key());
    assert_eq!(revoked.spaces[0].level, Some(GrantLevel::None));

    // The open connection is refused on its next call, and so is a new one.
    let refused = rpc(agent.recent(f.space("Work"), 5));
    assert_eq!(refused.code, codes::UNAUTHENTICATED);
    let refused = rpc(Client::connect_as_agent(f.socket(), &key));
    assert_eq!(refused.code, codes::UNAUTHENTICATED);
    assert!(refused.message.contains("revoked"), "{}", refused.message);

    // The space records the revocation, so it travels with it.
    let space = Space::open(f.space("Work")).unwrap();
    let id = &space.agents().unwrap()[0].id;
    assert!(space.agent_keys(id).unwrap()[0].revoked_at.is_some());
}

#[test]
fn a_new_key_replaces_the_old_one_and_keeps_the_grants() {
    let f = Fixture::new();
    let old = f.agent("claude-code");
    f.grant("claude-code", "Work", GrantLevel::Read);
    let new = f.agent("claude-code");
    assert_ne!(old, new);
    let refused = rpc(Client::connect_as_agent(f.socket(), &old));
    assert_eq!(refused.code, codes::UNAUTHENTICATED);
    let agent = f.connect(&new);
    agent.recent(f.space("Work"), 5).unwrap();
    let refused = rpc(agent.create_node(f.space("Work"), NewNode::note("x", ""), &me()));
    assert_eq!(refused.code, codes::FORBIDDEN);

    let agents = f.person.agents().unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].keys.len(), 2);
    assert!(agents[0].keys[0].revoked_at.is_some());
    assert!(agents[0].keys[1].revoked_at.is_none());
    assert_eq!(agents[0].spaces[0].level, Some(GrantLevel::Read));
}

#[test]
fn the_access_log_has_no_content_or_queries() {
    let f = Fixture::new();
    let key = f.agent("claude-code");
    f.grant("claude-code", "Work", GrantLevel::Read);
    let agent = f.connect(&key);
    agent
        .search(f.space("Work"), "a-very-secret-query", 5)
        .unwrap();
    let _ = agent.create_node(f.space("Work"), NewNode::note("Hidden title", ""), &me());
    let _ = agent.recent(f.space("Personal"), 5);
    let log = std::fs::read_to_string(f.dir.path().join("home").join(ACCESS_LOG_FILE)).unwrap();
    let lines: Vec<serde_json::Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let outcomes: Vec<(&str, &str)> = lines
        .iter()
        .map(|l| {
            (
                l["method"].as_str().unwrap(),
                l["outcome"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            ("auth", "allowed"),
            ("search", "allowed"),
            ("node.create", "denied"),
            ("recent", "invisible"),
        ]
    );
    assert!(lines[1]["space"].is_string() && lines[1]["agent"].is_string());
    for secret in ["a-very-secret-query", "Hidden title", &key] {
        assert!(!log.contains(secret), "{log}");
    }
    let registry = std::fs::read_to_string(f.dir.path().join("home/agents.json")).unwrap();
    assert!(!registry.contains(&key));
    assert!(registry.contains(&fingerprint(&key)));
}

#[test]
fn agents_read_history_and_undo_only_their_own_changes() {
    let f = Fixture::new();
    let work = f.space("Work");
    let claude = f.agent("claude-code");
    let codex = f.agent("codex");
    f.grant("claude-code", "Work", GrantLevel::Write);
    f.grant("codex", "Work", GrantLevel::Read);
    let claude = f.connect(&claude);
    let codex = f.connect(&codex);

    let mine = claude
        .create_node(
            &work,
            NewNode::note("Mine", "One."),
            &Provenance::agent("x"),
        )
        .unwrap();
    let theirs = f
        .person
        .create_node(&work, NewNode::note("Hers", "Two."), &me())
        .unwrap();
    let my_change = claude.changes_of(&work, &mine.id.to_string(), 1).unwrap()[0].id;
    let her_change = claude.changes_of(&work, &theirs.id.to_string(), 1).unwrap()[0].id;

    // Reading the history needs read; undo needs write and the agent's own change.
    assert!(codex.changes(&work, 10, None).unwrap().len() >= 2);
    assert!(codex.change(&work, my_change).unwrap().is_some());
    assert_eq!(
        rpc(codex.undo(&work, my_change, false, &me())).kind(),
        Some("forbidden")
    );
    let other = rpc(claude.undo(&work, her_change, false, &me()));
    assert_eq!(other.kind(), Some("forbidden"));
    assert!(
        other.message.contains("only undo their own"),
        "{}",
        other.message
    );
    assert_eq!(
        rpc(claude.undo(&work, my_change, true, &me())).kind(),
        Some("forbidden")
    );
    let undo = claude.undo(&work, my_change, false, &me()).unwrap();
    assert_eq!(undo.changed_by.actor, "claude-code");
    assert!(f.person.get_node(&work, &mine.id).unwrap().is_none());

    // Purge, import and the live folder: people only, or write.
    assert_eq!(
        rpc(claude.purge(&work, &theirs.id, &me())).kind(),
        Some("forbidden")
    );
    assert_eq!(
        rpc(claude.set_markdown_folder(&work, Some(&f.dir.path().join("md")), &me())).kind(),
        Some("forbidden")
    );
    assert_eq!(
        rpc(codex.import_markdown(&work, f.dir.path(), &me())).kind(),
        Some("forbidden")
    );
    assert!(codex.markdown_folder(&work).unwrap().folder.is_none());

    // A link removed by an agent without `by` is still recorded as that agent.
    let a = claude
        .create_node(&work, NewNode::note("A", ""), &me())
        .unwrap();
    let b = claude
        .create_node(&work, NewNode::note("B", ""), &me())
        .unwrap();
    claude.link(&work, &a.id, &b.id, "related", &me()).unwrap();
    claude
        .call(
            "link.delete",
            json!({ "space": work, "from": a.id, "to": b.id, "kind": "related" }),
        )
        .unwrap();
    let last = &f.person.changes(&work, 1, None).unwrap()[0];
    assert_eq!(last.changed_by.actor, "claude-code");
}
