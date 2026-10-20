//! Capture inside the service: synthetic agent sessions in temporary session folders become
//! conversation nodes of the default space, announced to subscribers like any other write.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use knode_format::{NewNode, Provenance, Space, kinds};
use knode_service::{
    Agent, CaptureOptions, ChangeKind, Client, Config, Event, ServiceHandle, WatchOptions, start,
};
use serde_json::json;
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(10);
const PROJECT: &str = "/Users/example/project";
const PRIVATE: &str = "/Users/example/private";

struct Fixture {
    dir: TempDir,
    service: Option<ServiceHandle>,
}

impl Fixture {
    /// A service with capture on, following session folders in a temporary folder, with a space
    /// that is not yet the default.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut fixture = Fixture { dir, service: None };
        fs::create_dir_all(fixture.claude()).unwrap();
        fs::create_dir_all(fixture.codex().join("2026/01/02")).unwrap();
        drop(Space::create(fixture.space(), "Personal").unwrap());
        let options = CaptureOptions {
            roots: Some(vec![
                (Agent::ClaudeCode, fixture.claude()),
                (Agent::Codex, fixture.codex()),
            ]),
            watch: WatchOptions {
                quiet: Duration::from_millis(100),
                max_delay: Duration::from_millis(500),
                rescan: None,
            },
            settings_poll: Duration::from_millis(50),
        };
        let config = Config::new(fixture.home()).with_capture(options);
        fixture.service = Some(start(config).unwrap());
        fixture
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn claude(&self) -> PathBuf {
        self.dir.path().join("claude/projects")
    }

    fn codex(&self) -> PathBuf {
        self.dir.path().join("codex/sessions")
    }

    fn space(&self) -> PathBuf {
        self.dir.path().join("Personal.knode")
    }

    fn client(&self) -> Client {
        Client::connect(self.home().join("service.sock")).unwrap()
    }

    fn write_config(&self, extra: &str) {
        let space = fs::canonicalize(self.space()).unwrap();
        let text = format!("default_space = {:?}\n{extra}", space.to_str().unwrap());
        fs::write(self.home().join("config.toml"), text).unwrap();
    }

    /// The session file of a Claude Code session in a project.
    fn session(&self, project: &str, session: &str) -> PathBuf {
        let dir = self.claude().join(project.replace('/', "-"));
        fs::create_dir_all(&dir).unwrap();
        dir.join(format!("{session}.jsonl"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(service) = self.service.take() {
            service.stop().unwrap();
        }
    }
}

/// One Claude Code record of a user or assistant message.
fn record(project: &str, session: &str, role: &str, minute: u32, text: &str) -> String {
    let message = if role == "assistant" {
        json!({"role": "assistant", "model": "claude-example-2",
               "content": [{"type": "text", "text": text}]})
    } else {
        json!({"role": "user", "content": text})
    };
    let line = json!({
        "type": role, "sessionId": session, "cwd": project, "gitBranch": "main",
        "isSidechain": false, "uuid": format!("{role}-{minute}"),
        "timestamp": format!("2026-01-02T10:{minute:02}:00.000Z"), "message": message,
    });
    format!("{line}\n")
}

fn append(path: &Path, text: &str) {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

/// The next `changed` event, which `what` describes.
fn next_change(events: &knode_service::Subscription, what: &str) -> knode_service::Change {
    match events.recv_timeout(WAIT) {
        Some(Event::Changed(change)) => change,
        other => panic!("expected a change for {what}, got {other:?}"),
    }
}

#[test]
fn captures_sessions_into_the_default_space_and_announces_them() {
    let fixture = Fixture::new();
    let first = fixture.session(PROJECT, "s-1");
    append(
        &first,
        &record(PROJECT, "s-1", "user", 0, "Where is the index?"),
    );
    append(
        &first,
        &record(PROJECT, "s-1", "assistant", 1, "In one SQLite file."),
    );
    let private = fixture.session(PRIVATE, "s-2");
    append(
        &private,
        &record(PRIVATE, "s-2", "user", 0, "A private question"),
    );

    let client = fixture.client();
    // No default space yet: capture waits.
    assert_eq!(client.status().unwrap().capture, None);
    let events = client.subscribe(None).unwrap();

    // Naming the default space in config.toml starts capture without a restart.
    fixture.write_config(&format!("capture_exclude = [{PRIVATE:?}]\n"));
    let created = next_change(&events, "the first session");
    assert_eq!(created.op, ChangeKind::Create);
    let space = fs::canonicalize(fixture.space()).unwrap();
    assert_eq!(created.space, space);
    assert_eq!(client.status().unwrap().capture, Some(space.clone()));
    // Let the file system watcher settle before writing.
    thread::sleep(Duration::from_millis(300));

    let node = client.get_node(&space, &created.node_id).unwrap().unwrap();
    assert_eq!(node.kind, kinds::CONVERSATION);
    assert!(node.body.contains("In one SQLite file."), "{}", node.body);
    assert_eq!(node.created_by.actor, "claude-code");
    assert_eq!(node.created_by.session.as_deref(), Some("s-1"));
    // The excluded project was left out.
    let hits = client.search(&space, "private", 10).unwrap();
    assert!(hits.is_empty(), "{hits:?}");
    assert!(fixture.home().join("capture/state.json").exists());

    // A client writes while capture follows the session: both go through the same space.
    let me = Provenance::person("tester");
    let note = client
        .create_node(&space, NewNode::note("Index notes", "Mine."), &me)
        .unwrap();
    assert_eq!(next_change(&events, "the note").node_id, note.id);

    // The session grows: the watcher updates the same node.
    append(&first, &record(PROJECT, "s-1", "user", 2, "And the blobs?"));
    let updated = next_change(&events, "the appended message");
    assert_eq!(updated.node_id, created.node_id);
    assert_eq!(updated.op, ChangeKind::Update);
    let node = client.get_node(&space, &created.node_id).unwrap().unwrap();
    assert!(node.body.contains("And the blobs?"), "{}", node.body);

    // A new session in another agent's folder is found too. (Its day folder already exists:
    // a file written into a folder created a moment earlier may only be found by the next
    // rescan, since the folder is watched once it is seen.)
    let codex_day = fixture.codex().join("2026/01/02");
    let rollout = Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "../knode-ingest/tests/fixtures/codex/sessions/2026/01/02/\
         rollout-2026-01-02T11-00-00-0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee.jsonl",
    );
    fs::copy(&rollout, codex_day.join(rollout.file_name().unwrap())).unwrap();
    let codex = next_change(&events, "the new Codex session");
    assert_eq!(codex.op, ChangeKind::Create);
    let node = client.get_node(&space, &codex.node_id).unwrap().unwrap();
    assert_eq!(node.created_by.actor, "codex");

    // capture = false stops it.
    fixture.write_config("capture = false\n");
    wait_until("capture to stop", || {
        client.status().unwrap().capture.is_none()
    });
    append(&first, &record(PROJECT, "s-1", "user", 3, "Still there?"));
    thread::sleep(Duration::from_millis(800));
    let node = client.get_node(&space, &created.node_id).unwrap().unwrap();
    assert!(!node.body.contains("Still there?"));
    assert!(events.try_recv().is_none());
}

#[test]
fn a_restarted_service_does_not_capture_twice() {
    let mut fixture = Fixture::new();
    let path = fixture.session(PROJECT, "s-1");
    append(&path, &record(PROJECT, "s-1", "user", 0, "Hello"));
    fixture.write_config("");
    let space = fs::canonicalize(fixture.space()).unwrap();
    let client = fixture.client();
    wait_until("the conversation", || {
        client.recent(&space, 10).unwrap().len() == 1
    });
    drop(client);
    fixture.service.take().unwrap().stop().unwrap();

    // Started again, it continues from its saved state.
    append(&path, &record(PROJECT, "s-1", "assistant", 1, "Hi"));
    let options = CaptureOptions {
        roots: Some(vec![(Agent::ClaudeCode, fixture.claude())]),
        watch: WatchOptions::default(),
        settings_poll: Duration::from_millis(50),
    };
    fixture.service = Some(start(Config::new(fixture.home()).with_capture(options)).unwrap());
    let client = fixture.client();
    wait_until("the update", || {
        let recent = client.recent(&space, 10).unwrap();
        recent.len() == 1
            && client
                .get_node(&space, &recent[0].id)
                .unwrap()
                .is_some_and(|n| n.body.contains("Hi"))
    });
    assert_eq!(client.recent(&space, 10).unwrap().len(), 1);
}

fn me() -> Provenance {
    Provenance::person("tester")
}

/// The conversations of a space, by title.
fn conversations(client: &Client, space: &Path) -> Vec<String> {
    client
        .recent(space, 100)
        .unwrap()
        .into_iter()
        .filter(|n| n.kind == kinds::CONVERSATION)
        .map(|n| n.title)
        .collect()
}

#[test]
fn routes_conversations_by_the_folders_of_every_space() {
    let fixture = Fixture::new();
    let work_path = fixture.dir.path().join("Work.knode");
    drop(Space::create(&work_path, "Work").unwrap());
    let work = fs::canonicalize(&work_path).unwrap();
    let personal = fs::canonicalize(fixture.space()).unwrap();
    let client = fixture.client();
    let project = |path: &str, capture: bool| knode_service::FolderInput {
        id: None,
        kind: "project".into(),
        path: path.into(),
        capture,
        rules: json!({ "exclude": ["scratch/**"] }),
    };
    // Work is fed by /Users/example/work, but not by its secret folder.
    client
        .set_folder(&work, &project("/Users/example/work", true), &me())
        .unwrap();
    let secret = client
        .set_folder(&work, &project("/Users/example/work/secret", false), &me())
        .unwrap();

    let at_work = "/Users/example/work/app";
    append(
        &fixture.session(at_work, "w-1"),
        &record(at_work, "w-1", "user", 0, "Plan the work release"),
    );
    append(
        &fixture.session(PROJECT, "p-1"),
        &record(PROJECT, "p-1", "user", 0, "Plan a holiday"),
    );
    let hidden = "/Users/example/work/secret/x";
    let hidden_session = fixture.session(hidden, "s-1");
    append(
        &hidden_session,
        &record(hidden, "s-1", "user", 0, "Secret plans"),
    );
    let scratch = "/Users/example/work/scratch/tmp";
    append(
        &fixture.session(scratch, "t-1"),
        &record(scratch, "t-1", "user", 0, "Scratch plans"),
    );

    // The service knows Work from config.toml, and Personal is the default.
    fixture.write_config(&format!("spaces = [{:?}]\n", work.to_str().unwrap()));
    wait_until("both spaces to have their conversation", || {
        conversations(&client, &personal).len() == 1 && conversations(&client, &work).len() == 1
    });
    thread::sleep(Duration::from_millis(500));
    assert_eq!(conversations(&client, &work), vec!["Plan the work release"]);
    assert_eq!(conversations(&client, &personal), vec!["Plan a holiday"]);
    // Capture off for a project, or excluded by its rules: stored nowhere at all.
    for space in [&work, &personal] {
        assert!(client.search(space, "secret", 10).unwrap().is_empty());
        assert!(client.search(space, "scratch", 10).unwrap().is_empty());
    }
    let state = fs::read_to_string(fixture.home().join("capture/state.json")).unwrap();
    assert!(!state.contains("Secret plans"));

    // Turning capture on for the secret folder takes effect without a restart.
    let on = knode_service::FolderInput {
        id: Some(secret.folder.id.clone()),
        ..project("/Users/example/work/secret", true)
    };
    client.set_folder(&work, &on, &me()).unwrap();
    wait_until("the secret conversation in Work", || {
        conversations(&client, &work).len() == 2
    });
    assert_eq!(conversations(&client, &personal).len(), 1);

    // An agent folder of Work: its conversations of no project go to Work.
    let team = fixture.dir.path().join("agents/team");
    let elsewhere = "/Users/example/elsewhere";
    let dir = team.join(elsewhere.replace('/', "-"));
    fs::create_dir_all(&dir).unwrap();
    append(
        &dir.join("a-1.jsonl"),
        &record(elsewhere, "a-1", "user", 0, "Team notes"),
    );
    let agent_folder = knode_service::FolderInput {
        id: None,
        kind: "agent".into(),
        path: team.to_string_lossy().into_owned(),
        capture: true,
        rules: json!({ "agent": "claude-code" }),
    };
    client.set_folder(&work, &agent_folder, &me()).unwrap();
    wait_until("the agent folder conversation in Work", || {
        conversations(&client, &work).len() == 3
    });
    assert_eq!(conversations(&client, &personal).len(), 1);

    // Capture off for the whole of Work: its conversations are no longer written anywhere.
    client
        .set_setting(&work, "capture.enabled", Some("off"), &me())
        .unwrap();
    thread::sleep(Duration::from_millis(300));
    append(
        &hidden_session,
        &record(hidden, "s-1", "user", 1, "More secret plans"),
    );
    thread::sleep(Duration::from_millis(1000));
    for space in [&work, &personal] {
        assert!(client.search(space, "more", 10).unwrap().is_empty());
    }
    assert_eq!(conversations(&client, &personal).len(), 1);
}
