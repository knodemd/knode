//! Editing a body together through Knode Service: a person typing with `doc.edit` splices and
//! agents rewriting the whole text with `node.update`, at once, merged with Loro.

use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use knode_format::{ActorKind, GrantLevel, NewNode, NodeId, NodeUpdate, Provenance};
use knode_service::keys::{fingerprint, generate_key};
use knode_service::protocol::codes;
use knode_service::{
    ChangeKind, Client, Config, DocSplice, Error, Event, ServiceHandle, Subscription, start,
};
use knode_sync::loro::{ExportMode, LoroDoc, VersionVector};
use serde_json::json;
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(10);

struct Fixture {
    dir: TempDir,
    _service: ServiceHandle,
    person: Client,
}

fn ada() -> Provenance {
    Provenance::person("Ada")
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let service = start(Config::new(dir.path().join("home"))).unwrap();
        let person = Client::connect(service.socket_path()).unwrap();
        let fixture = Fixture {
            dir,
            _service: service,
            person,
        };
        fixture
            .person
            .create_space(fixture.space(), "Work")
            .unwrap();
        fixture
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join("home/service.sock")
    }

    fn space(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path())
            .unwrap()
            .join("Work.knode")
    }

    /// Adds an agent that may write the space, and connects as it.
    fn agent(&self, name: &str, level: GrantLevel) -> Client {
        let key = generate_key().unwrap();
        self.person
            .add_agent(name, name, &fingerprint(&key), &ada())
            .unwrap();
        self.person
            .set_grant(name, self.space(), level, &ada())
            .unwrap();
        Client::connect_as_agent(self.socket(), &key).unwrap()
    }

    fn note(&self, body: &str) -> NodeId {
        self.person
            .create_node(self.space(), NewNode::note("Plan", body), &ada())
            .unwrap()
            .id
    }

    fn body(&self, id: &NodeId) -> String {
        self.person
            .get_node(self.space(), id)
            .unwrap()
            .unwrap()
            .body
    }

    fn doc_dir(&self, id: &NodeId) -> PathBuf {
        self.space().join("sync/nodes").join(id.to_string())
    }
}

fn rpc(result: Result<impl std::fmt::Debug, Error>) -> knode_service::RpcError {
    match result {
        Err(Error::Rpc(e)) => e,
        other => panic!("expected an error from the service, got {other:?}"),
    }
}

fn insert(at: usize, text: &str) -> DocSplice {
    DocSplice {
        at,
        delete: 0,
        insert: text.into(),
    }
}

/// An editor as the app keeps one: its text, the version its text is at, and the version the
/// body is at in the service.
struct Editor {
    text: String,
    base: String,
}

impl Editor {
    fn open(client: &Client, space: &PathBuf, id: &NodeId) -> Self {
        let open = client.open_doc(space, id).unwrap();
        Editor {
            text: open.text,
            base: open.version,
        }
    }

    /// Types `word` at the start, sends it, and catches up with what others wrote.
    fn type_at_start(&mut self, client: &Client, space: &PathBuf, id: &NodeId, word: &str) {
        let splice = insert(0, word);
        self.text.insert_str(0, word);
        let edited = client
            .edit_doc(space, id, Some(&self.base), &[splice], &ada())
            .unwrap();
        // With no edit waiting, the editor shows what others wrote.
        match edited.text {
            Some(text) => {
                self.text = text;
                self.base = edited.version;
            }
            None => self.base = edited.seen,
        }
    }
}

#[test]
fn a_person_typing_and_two_agents_rewriting_converge_with_nothing_lost() {
    let f = Fixture::new();
    let space = f.space();
    let id = f.note("# Plan\n\nShip v0.3.\n");
    let claude = f.agent("claude-code", GrantLevel::Write);
    let codex = f.agent("codex", GrantLevel::Write);
    let rounds = 15;
    let start = Arc::new(Barrier::new(3));

    let typing = {
        let (start, space) = (start.clone(), space.clone());
        let socket = f.socket();
        thread::spawn(move || {
            let client = Client::connect(socket).unwrap();
            let mut editor = Editor::open(&client, &space, &id);
            start.wait();
            for i in 0..rounds {
                editor.type_at_start(&client, &space, &id, &format!("p{i} "));
            }
            editor
        })
    };
    let agents: Vec<_> = [("claude-code", claude), ("codex", codex)]
        .into_iter()
        .map(|(name, client)| {
            let (start, space) = (start.clone(), space.clone());
            thread::spawn(move || {
                start.wait();
                for i in 0..rounds {
                    // An agent reads the note, then rewrites all of it from what it read.
                    let read = client.open_doc(&space, &id).unwrap();
                    let text = format!("{}\n{name} line {i}", read.text.trim_end());
                    let update = NodeUpdate {
                        body: Some(text),
                        ..Default::default()
                    };
                    let (node, edited) = client
                        .update_node_from(&space, &id, update, Some(&read.version), &ada())
                        .unwrap();
                    assert_eq!(node.updated_by.kind, ActorKind::Agent);
                    assert!(edited.is_some(), "the body went through its document");
                }
            })
        })
        .collect();
    for agent in agents {
        agent.join().unwrap();
    }
    let mut editor = typing.join().unwrap();

    let body = f.body(&id);
    for i in 0..rounds {
        assert!(body.contains(&format!("p{i} ")), "lost p{i} in {body:?}");
        for name in ["claude-code", "codex"] {
            assert!(
                body.contains(&format!("{name} line {i}")),
                "lost {name} line {i} in {body:?}"
            );
        }
    }
    assert!(body.contains("Ship v0.3."));
    // The editor catches up once it has nothing waiting: it shows the same text.
    let caught_up = f
        .person
        .edit_doc(&space, &id, Some(&editor.base), &[], &ada())
        .unwrap();
    if let Some(text) = caught_up.text {
        editor.text = text;
    }
    assert_eq!(editor.text, body);
    assert_eq!(f.person.open_doc(&space, &id).unwrap().text, body);

    // Every write is its own change, by whoever made it.
    let history = f.person.history(&space, &id).unwrap();
    let by = |actor: &str| {
        history
            .iter()
            .filter(|h| h.changed_by.actor == actor)
            .count()
    };
    assert_eq!(by("claude-code"), rounds);
    assert_eq!(by("codex"), rounds);
    assert_eq!(by("Ada"), rounds + 1);
}

#[test]
fn an_agent_writing_from_an_old_version_keeps_what_the_person_typed() {
    let f = Fixture::new();
    let space = f.space();
    let id = f.note("Ship v0.3.");
    let claude = f.agent("claude-code", GrantLevel::Write);

    let read = claude.open_doc(&space, &id).unwrap();
    let person = f.person.open_doc(&space, &id).unwrap();
    let typed = f
        .person
        .edit_doc(
            &space,
            &id,
            Some(&person.version),
            &[insert(0, "Soon: ")],
            &ada(),
        )
        .unwrap();
    assert_eq!(typed.text, None, "nobody else wrote");
    assert_eq!(typed.seen, typed.version);

    let update = NodeUpdate {
        body: Some("Ship v0.3 on Friday.".into()),
        ..Default::default()
    };
    let (node, edited) = claude
        .update_node_from(&space, &id, update, Some(&read.version), &ada())
        .unwrap();
    assert_eq!(node.body, "Soon: Ship v0.3 on Friday.");
    let edited = edited.unwrap();
    assert_ne!(edited.version, edited.seen, "the agent's edit was merged");
    assert_eq!(edited.text.as_deref(), Some("Soon: Ship v0.3 on Friday."));
}

#[test]
fn old_clients_without_a_version_keep_working() {
    let f = Fixture::new();
    let space = f.space();
    let id = f.note("one");

    // A node without a document is written as before, and gets none.
    let raw = f
        .person
        .call(
            "node.update",
            json!({ "space": space, "id": id, "body": "two", "by": ada() }),
        )
        .unwrap();
    assert_eq!(raw["body"], "two");
    assert!(raw.get("version").is_none());
    assert!(!f.doc_dir(&id).exists());

    // Once someone edits it together, a body without a version replaces the text, through the
    // document: an open editor's next edit still merges.
    let editor = f.person.open_doc(&space, &id).unwrap();
    assert!(f.doc_dir(&id).is_dir());
    let raw = f
        .person
        .call(
            "node.update",
            json!({ "space": space, "id": id, "body": "two three", "by": ada() }),
        )
        .unwrap();
    assert_eq!(raw["body"], "two three");
    assert!(raw["version"].is_string());
    let edited = f
        .person
        .edit_doc(
            &space,
            &id,
            Some(&editor.version),
            &[insert(0, "Now: ")],
            &ada(),
        )
        .unwrap();
    assert_eq!(f.body(&id), "Now: two three");
    assert_eq!(edited.text.as_deref(), Some("Now: two three"));

    // A title-only update leaves the body and its document alone.
    let raw = f
        .person
        .call(
            "node.update",
            json!({ "space": space, "id": id, "title": "Renamed", "by": ada() }),
        )
        .unwrap();
    assert_eq!(raw["title"], "Renamed");
    assert_eq!(raw["body"], "Now: two three");
}

#[test]
fn changed_carries_the_version_and_the_writer_and_updates_on_request() {
    let f = Fixture::new();
    let space = f.space();
    let id = f.note("hello");
    let plain = Client::connect(f.socket()).unwrap();
    let plain_events = plain.subscribe(Some(&space)).unwrap();
    let editor = Client::connect(f.socket()).unwrap();
    let editor_events = editor.subscribe_with(Some(&space), true).unwrap();

    let open = f.person.open_doc(&space, &id).unwrap();
    let edited = f
        .person
        .edit_doc(
            &space,
            &id,
            Some(&open.version),
            &[insert(5, " world")],
            &ada(),
        )
        .unwrap();

    let change = |events: &Subscription| loop {
        match events.recv_timeout(WAIT) {
            Some(Event::Changed(change)) if change.version.is_some() => return change,
            Some(_) => continue,
            None => panic!("no changed with a version"),
        }
    };
    let seen = change(&plain_events);
    assert_eq!(seen.node_id, id);
    assert_eq!(seen.op, ChangeKind::Update);
    assert_eq!(seen.version.as_deref(), Some(edited.version.as_str()));
    assert_eq!(seen.by, Some(ada()));
    assert_eq!(seen.update, None, "updates only for those who ask");
    let with_update = change(&editor_events);
    let bytes = BASE64.decode(with_update.update.unwrap()).unwrap();

    // The update brings a copy at the old version up to the new one.
    let copy = LoroDoc::new();
    let all = BASE64
        .decode(
            f.person
                .call(
                    "doc.updates",
                    json!({ "space": space, "id": id, "since": open.version }),
                )
                .unwrap()["update"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
    assert!(!all.is_empty());
    let whole = BASE64
        .decode(
            f.person
                .call("doc.updates", json!({ "space": space, "id": id }))
                .unwrap()["update"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
    copy.import(&whole).unwrap();
    copy.import(&bytes).unwrap();
    assert_eq!(copy.get_text("body").to_string(), "hello world");
}

#[test]
fn an_editor_with_its_own_copy_merges_updates_as_its_peer() {
    let f = Fixture::new();
    let space = f.space();
    let id = f.note("draft");
    let peer: u64 = f
        .person
        .call("doc.peer", json!({ "space": space, "by": ada() }))
        .unwrap()["peer"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let whole = f
        .person
        .call("doc.updates", json!({ "space": space, "id": id }))
        .unwrap();
    let copy = LoroDoc::new();
    copy.import(&BASE64.decode(whole["update"].as_str().unwrap()).unwrap())
        .unwrap();
    let before: VersionVector = copy.oplog_vv();
    copy.set_peer_id(peer).unwrap();
    copy.get_text("body").insert(5, " two").unwrap();
    copy.commit();
    let update = copy.export(ExportMode::updates(&before)).unwrap();
    let merged = f
        .person
        .call(
            "doc.merge",
            json!({ "space": space, "id": id, "update": BASE64.encode(update) }),
        )
        .unwrap();
    assert!(merged["version"].is_string());
    let node = f.person.get_node(&space, &id).unwrap().unwrap();
    assert_eq!(node.body, "draft two");
    assert_eq!(node.updated_by, ada(), "authorship comes from the peer");
}

#[test]
fn agents_edit_with_a_write_grant_but_never_merge_raw_updates() {
    let f = Fixture::new();
    let space = f.space();
    let id = f.note("hello");
    let reader = f.agent("reader", GrantLevel::Read);
    let writer = f.agent("writer", GrantLevel::Write);

    let open = reader.open_doc(&space, &id).unwrap();
    assert_eq!(open.text, "hello");
    let refused = rpc(reader.edit_doc(&space, &id, None, &[insert(0, "x")], &ada()));
    assert_eq!(refused.code, codes::FORBIDDEN);

    let edited = writer
        .edit_doc(&space, &id, Some(&open.version), &[insert(5, "!")], &ada())
        .unwrap();
    assert!(edited.text.is_none());
    let node = f.person.get_node(&space, &id).unwrap().unwrap();
    assert_eq!(node.body, "hello!");
    assert_eq!(node.updated_by.actor, "writer", "the key decides who wrote");

    for method in ["doc.peer", "doc.merge"] {
        let refused = rpc(writer.call(
            method,
            json!({ "space": space, "id": id, "update": "", "by": ada() }),
        ));
        assert_eq!(refused.code, codes::FORBIDDEN, "{method}");
    }
}

#[test]
fn bad_edits_are_refused_with_a_reason() {
    let f = Fixture::new();
    let space = f.space();
    let id = f.note("abc");
    f.person.open_doc(&space, &id).unwrap();

    let unknown = rpc(f.person.edit_doc(
        &space,
        &id,
        Some(r#"{"12345":9}"#),
        &[insert(0, "x")],
        &ada(),
    ));
    assert_eq!(unknown.code, codes::UNKNOWN_VERSION);
    assert_eq!(unknown.kind(), Some("unknown_version"));

    let past_the_end = rpc(f
        .person
        .edit_doc(&space, &id, None, &[insert(9, "x")], &ada()));
    assert_eq!(past_the_end.code, codes::INVALID);

    let both = rpc(f.person.call(
        "doc.edit",
        json!({ "space": space, "id": id, "text": "x", "splices": [], "by": ada() }),
    ));
    assert_eq!(both.code, codes::INVALID_PARAMS);
    let not_a_version = rpc(f.person.edit_doc(&space, &id, Some("v1"), &[], &ada()));
    assert_eq!(not_a_version.code, codes::INVALID_PARAMS);
    assert_eq!(f.body(&id), "abc");
}

#[test]
fn purging_a_node_removes_its_document_and_deleting_keeps_it() {
    let f = Fixture::new();
    let space = f.space();
    let id = f.note("secret");
    let open = f.person.open_doc(&space, &id).unwrap();
    f.person
        .edit_doc(
            &space,
            &id,
            Some(&open.version),
            &[insert(0, "top ")],
            &ada(),
        )
        .unwrap();
    assert!(f.doc_dir(&id).is_dir());

    f.person.delete_node(&space, &id, &ada()).unwrap();
    assert!(f.doc_dir(&id).is_dir(), "a deleted node keeps its document");
    f.person.purge(&space, &id, &ada()).unwrap();
    assert!(!f.doc_dir(&id).exists(), "a purged node has no document");
}

#[test]
fn a_restore_is_followed_by_the_open_editor() {
    let f = Fixture::new();
    let space = f.space();
    let id = f.note("first");
    let open = f.person.open_doc(&space, &id).unwrap();
    f.person
        .edit_doc(
            &space,
            &id,
            Some(&open.version),
            &[insert(5, " second")],
            &ada(),
        )
        .unwrap();
    let history = f.person.history(&space, &id).unwrap();
    f.person
        .restore(&space, &id, history[0].seq, &ada())
        .unwrap();
    assert_eq!(f.body(&id), "first");
    // The document is brought in step with the restored body before the next edit (21.5).
    let reopened = f.person.open_doc(&space, &id).unwrap();
    assert_eq!(reopened.text, "first");
    let edited = f
        .person
        .edit_doc(
            &space,
            &id,
            Some(&reopened.version),
            &[insert(0, "1. ")],
            &ada(),
        )
        .unwrap();
    assert_eq!(edited.text, None);
    assert_eq!(f.body(&id), "1. first");
}

/// How long editing together takes through the service on a 50 KB note: run with
/// `cargo test --release -p knode-service --test coediting -- --ignored --nocapture`.
#[test]
#[ignore]
fn timings_on_a_50_kb_note() {
    let f = Fixture::new();
    let space = f.space();
    let paragraph = "A paragraph of a long note, written to see how long edits take. ";
    let body: String = (0..800).map(|i| format!("{paragraph}{i}\n")).collect();
    assert!(body.len() > 50_000);
    let id = f.note(&body);
    let claude = f.agent("claude-code", GrantLevel::Write);

    let started = std::time::Instant::now();
    let open = f.person.open_doc(&space, &id).unwrap();
    println!("doc.open, first time: {:?}", started.elapsed());

    let mut base = open.version;
    let edits = 100;
    let started = std::time::Instant::now();
    for i in 0..edits {
        let edited = f
            .person
            .edit_doc(&space, &id, Some(&base), &[insert(i * 7, "typed ")], &ada())
            .unwrap();
        base = edited.seen;
    }
    println!(
        "doc.edit, one batch of typing: {:?} each",
        started.elapsed() / edits as u32
    );

    let read = claude.open_doc(&space, &id).unwrap();
    f.person
        .edit_doc(&space, &id, Some(&base), &[insert(0, "late ")], &ada())
        .unwrap();
    let rewritten = read.text.replace("long note", "longer note");
    let started = std::time::Instant::now();
    let update = NodeUpdate {
        body: Some(rewritten),
        ..Default::default()
    };
    claude
        .update_node_from(&space, &id, update, Some(&read.version), &ada())
        .unwrap();
    println!(
        "node.update, an agent's full rewrite merged from an old version: {:?}",
        started.elapsed()
    );
    assert!(f.body(&id).starts_with("late "));
}
