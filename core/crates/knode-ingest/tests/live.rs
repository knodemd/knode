//! Capture into a space: synthetic session files written, grown, cut and filtered, synced with
//! `Capture` and followed with `Watcher`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use knode_format::{ActorKind, ChangeOp, Space, kinds, property_keys};
use knode_ingest::{Agent, Capture, CaptureReport, ProjectFilter, WatchOptions, Watcher};
use serde_json::json;
use tempfile::TempDir;

const SESSION: &str = "5e5e5e5e-1111-4222-8333-444455556666";
const PROJECT: &str = "/Users/example/project";
const PRIVATE: &str = "/Users/example/private";

/// A temporary home with session folders, a space and a state file.
struct Home {
    dir: TempDir,
    space: Space,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
        fs::create_dir_all(dir.path().join("claude")).unwrap();
        fs::create_dir_all(dir.path().join("codex")).unwrap();
        Self { dir, space }
    }

    fn claude_root(&self) -> PathBuf {
        self.dir.path().join("claude")
    }

    fn codex_root(&self) -> PathBuf {
        self.dir.path().join("codex")
    }

    fn state(&self) -> PathBuf {
        self.dir.path().join("capture/state.json")
    }

    fn capture(&self) -> Capture {
        Capture::open(self.state()).unwrap().with_roots([
            (Agent::ClaudeCode, self.claude_root()),
            (Agent::Codex, self.codex_root()),
        ])
    }

    /// The path of a Claude Code session of `project`.
    fn session_path(&self, project: &str, session: &str) -> PathBuf {
        let dir: String = project
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let dir = self.claude_root().join(dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join(format!("{session}.jsonl"))
    }

    fn copy_fixtures(&self) {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        copy_dir(&fixtures.join("claude/projects"), &self.claude_root());
        copy_dir(&fixtures.join("codex/sessions"), &self.codex_root());
    }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
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

/// A session with one question and its answer.
fn start_session(home: &Home, project: &str, session: &str) -> PathBuf {
    let path = home.session_path(project, session);
    append(
        &path,
        &record(project, session, "user", 0, "How do we store the index?"),
    );
    append(
        &path,
        &record(project, session, "assistant", 1, "In one SQLite file."),
    );
    path
}

fn conversations(space: &Space) -> Vec<knode_format::NodeSummary> {
    space
        .recent(1000)
        .unwrap()
        .into_iter()
        .filter(|n| n.kind == kinds::CONVERSATION)
        .collect()
}

fn only_change(report: &CaptureReport) -> &knode_ingest::Captured {
    assert_eq!(report.changes.len(), 1, "{report:?}");
    &report.changes[0]
}

#[test]
fn writes_one_node_per_conversation_with_its_metadata() {
    let mut home = Home::new();
    home.copy_fixtures();
    let mut capture = home.capture();
    let report = capture.sync_all(&mut home.space).unwrap();

    // Two Claude Code sessions (the subagent transcript is not one) and two Codex sessions.
    assert_eq!(report.files, 4);
    assert_eq!(report.created().count(), 4, "{report:?}");
    assert_eq!(conversations(&home.space).len(), 4);

    let found = home
        .space
        .find_by_property(
            property_keys::CONVERSATION_SESSION,
            "0f8e2c1a-1111-4222-8333-444455556666",
        )
        .unwrap();
    assert_eq!(found.len(), 1);
    let node = home.space.get_node(&found[0].id).unwrap().unwrap();
    assert_eq!(node.kind, kinds::CONVERSATION);
    assert!(node.body.starts_with(&format!("# {}\n", node.title)));
    assert!(!node.body.starts_with("---"), "no frontmatter");
    let properties = home.space.properties(&node.id).unwrap();
    assert_eq!(properties[property_keys::CONVERSATION_AGENT], "claude-code");
    assert_eq!(properties[property_keys::CONVERSATION_PROJECT], PROJECT);
    assert_eq!(properties[property_keys::CONVERSATION_BRANCH], "main");
    assert_eq!(
        properties[property_keys::CONVERSATION_MODEL],
        "claude-example-2"
    );
    assert_eq!(
        properties[property_keys::CONVERSATION_STARTED_AT],
        "2026-01-02T10:00:00.000Z"
    );
    assert!(properties.contains_key(property_keys::CONVERSATION_ENDED_AT));

    let by = &node.created_by;
    assert_eq!(by.kind, ActorKind::Agent);
    assert_eq!(by.actor, "claude-code");
    assert_eq!(
        by.session.as_deref(),
        Some("0f8e2c1a-1111-4222-8333-444455556666")
    );
    assert!(by.source.as_deref().unwrap().ends_with(".jsonl"));

    // Codex keys by the id inside the file.
    let codex = home
        .space
        .find_by_property(
            property_keys::CONVERSATION_SESSION,
            "0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee",
        )
        .unwrap();
    assert_eq!(codex.len(), 1);
    assert_eq!(codex[0].updated_by.actor, "codex");

    // Nothing changed: nothing is written.
    let again = capture.sync_all(&mut home.space).unwrap();
    assert!(again.changes.is_empty(), "{again:?}");
    assert_eq!(again.unchanged, 4);
}

#[test]
fn a_growing_session_updates_the_same_node() {
    let mut home = Home::new();
    let path = start_session(&home, PROJECT, SESSION);
    let mut capture = home.capture();
    let created = capture.sync_all(&mut home.space).unwrap();
    let first = only_change(&created).clone();
    assert_eq!(first.op, ChangeOp::Create);
    assert_eq!(first.session_id, SESSION);

    append(
        &path,
        &record(PROJECT, SESSION, "user", 2, "And the full-text search?"),
    );
    append(
        &path,
        &record(PROJECT, SESSION, "assistant", 3, "FTS5, in the same file."),
    );
    let updated = capture.sync_paths(&mut home.space, &[&path]).unwrap();
    let change = only_change(&updated);
    assert_eq!(change.op, ChangeOp::Update);
    assert_eq!(change.node_id, first.node_id);

    assert_eq!(conversations(&home.space).len(), 1);
    let node = home.space.get_node(&first.node_id).unwrap().unwrap();
    assert!(node.body.contains("FTS5, in the same file."));
    assert_eq!(node.title, "How do we store the index?");
    assert_eq!(
        home.space
            .property(&node.id, property_keys::CONVERSATION_ENDED_AT)
            .unwrap()
            .as_deref(),
        Some("2026-01-02T10:03:00.000Z")
    );
    assert_eq!(home.space.history(&node.id).unwrap().len(), 2);

    // A path that is not a session file is ignored.
    let other = home.claude_root().join("notes.txt");
    fs::write(&other, "x").unwrap();
    let ignored = capture.sync_paths(&mut home.space, &[&other]).unwrap();
    assert_eq!(ignored.files, 0);
}

#[test]
fn continues_from_the_saved_state_after_a_restart() {
    let mut home = Home::new();
    let path = start_session(&home, PROJECT, SESSION);
    let node_id = {
        let mut capture = home.capture();
        let report = capture.sync_all(&mut home.space).unwrap();
        only_change(&report).node_id
    };
    let state = fs::read_to_string(home.state()).unwrap();
    assert!(state.contains(SESSION));

    // Change bytes already read, keeping the length: a resumed read never sees them.
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("SQLite", "SQLITE")).unwrap();
    append(&path, &record(PROJECT, SESSION, "user", 2, "Thanks."));

    let mut capture = home.capture();
    let report = capture.sync_all(&mut home.space).unwrap();
    let change = only_change(&report);
    assert_eq!((change.node_id, change.op), (node_id, ChangeOp::Update));
    let body = home.space.get_node(&node_id).unwrap().unwrap().body;
    assert!(body.contains("In one SQLite file."), "{body}");
    assert!(body.contains("Thanks."));
    assert!(!body.contains("SQLITE"));
}

#[test]
fn reads_a_cut_or_replaced_file_again_from_the_start() {
    let mut home = Home::new();
    let path = start_session(&home, PROJECT, SESSION);
    let mut capture = home.capture();
    let node_id = only_change(&capture.sync_all(&mut home.space).unwrap()).node_id;

    fs::write(
        &path,
        record(PROJECT, SESSION, "user", 5, "Start over: what is a node?"),
    )
    .unwrap();
    let report = capture.sync_paths(&mut home.space, &[&path]).unwrap();
    assert_eq!(report.restarted, 1);
    let change = only_change(&report);
    assert_eq!((change.node_id, change.op), (node_id, ChangeOp::Update));
    let node = home.space.get_node(&node_id).unwrap().unwrap();
    assert_eq!(node.title, "Start over: what is a node?");
    assert!(!node.body.contains("SQLite"));
    assert_eq!(conversations(&home.space).len(), 1);
}

#[test]
fn leaves_out_filtered_projects_and_reads_them_when_allowed() {
    let mut home = Home::new();
    start_session(&home, PROJECT, SESSION);
    let private = start_session(&home, PRIVATE, "6e6e6e6e-1111-4222-8333-444455556666");
    // A project inside the excluded one, which only the records reveal.
    let nested = start_session(
        &home,
        "/Users/example/private/app",
        "7e7e7e7e-1111-4222-8333-444455556666",
    );

    let mut capture = home
        .capture()
        .with_filter(ProjectFilter::excluding([PRIVATE]));
    let report = capture.sync_all(&mut home.space).unwrap();
    assert_eq!(report.created().count(), 1);
    assert_eq!(report.filtered, 2);
    // The text of a filtered conversation is not kept in the state; all three say the same.
    let state = fs::read_to_string(home.state()).unwrap();
    assert_eq!(state.matches("How do we store the index?").count(), 1);

    // Growing does not bring it in.
    append(
        &nested,
        &record("/Users/example/private/app", "x", "user", 2, "More."),
    );
    let report = capture.sync_all(&mut home.space).unwrap();
    assert!(report.changes.is_empty());

    // Without the filter, after a restart, both are read again from the start.
    drop(capture);
    let mut capture = home.capture();
    let report = capture.sync_all(&mut home.space).unwrap();
    assert_eq!(report.created().count(), 2, "{report:?}");
    assert_eq!(conversations(&home.space).len(), 3);
    assert!(private.exists());
}

#[test]
fn waits_for_the_first_message_and_respects_deleted_nodes() {
    let mut home = Home::new();
    let path = home.session_path(PROJECT, SESSION);
    append(
        &path,
        &format!(
            "{}\n",
            json!({"type": "permission-mode", "sessionId": SESSION})
        ),
    );
    let mut capture = home.capture();
    let report = capture.sync_all(&mut home.space).unwrap();
    assert_eq!(report.empty, 1);
    assert!(conversations(&home.space).is_empty());

    append(&path, &record(PROJECT, SESSION, "user", 1, "Hello"));
    let report = capture.sync_paths(&mut home.space, &[&path]).unwrap();
    let node_id = only_change(&report).node_id;

    // The person deletes the conversation: it does not come back as the session grows.
    home.space
        .delete_node(&node_id, &knode_format::Provenance::person("Ada"))
        .unwrap();
    append(&path, &record(PROJECT, SESSION, "assistant", 2, "Hi"));
    let report = capture.sync_paths(&mut home.space, &[&path]).unwrap();
    assert_eq!(report.deleted, 1);
    assert!(conversations(&home.space).is_empty());

    // A removed file is forgotten.
    fs::remove_file(&path).unwrap();
    let report = capture.sync_paths(&mut home.space, &[&path]).unwrap();
    assert_eq!(report.files, 0);
    assert!(!fs::read_to_string(home.state()).unwrap().contains(SESSION));
}

#[test]
fn a_different_space_gets_every_conversation() {
    let mut home = Home::new();
    start_session(&home, PROJECT, SESSION);
    let mut capture = home.capture();
    assert_eq!(capture.sync_all(&mut home.space).unwrap().changes.len(), 1);

    let mut other = Space::create(home.dir.path().join("Other.knode"), "Other").unwrap();
    let report = capture.sync_all(&mut other).unwrap();
    assert_eq!(report.created().count(), 1);
    assert_eq!(conversations(&other).len(), 1);
}

#[test]
fn a_damaged_state_file_starts_again_without_duplicates() {
    let mut home = Home::new();
    let path = start_session(&home, PROJECT, SESSION);
    home.capture().sync_all(&mut home.space).unwrap();
    fs::write(home.state(), "not json").unwrap();
    append(&path, &record(PROJECT, SESSION, "user", 2, "Again."));
    let report = home.capture().sync_all(&mut home.space).unwrap();
    assert_eq!(only_change(&report).op, ChangeOp::Update);
    assert_eq!(conversations(&home.space).len(), 1);
}

#[test]
fn the_watcher_follows_a_session_live() {
    let home = Home::new();
    let path = start_session(&home, PROJECT, SESSION);
    let space = Arc::new(Mutex::new(
        Space::open(home.dir.path().join("Test.knode")).unwrap(),
    ));
    let capture = Arc::new(Mutex::new(home.capture()));
    let first = capture
        .lock()
        .unwrap()
        .sync_all(&mut space.lock().unwrap())
        .unwrap();
    let node_id = only_change(&first).node_id;

    let (sender, reports) = mpsc::channel();
    let options = WatchOptions {
        quiet: Duration::from_millis(200),
        max_delay: Duration::from_secs(2),
        rescan: None,
    };
    let watcher = Watcher::capture(capture, space.clone(), options, move |result| {
        let _ = sender.send(result.unwrap());
    })
    .unwrap();
    assert_eq!(watcher.watched().len(), 2);
    // Let the file system watcher settle before writing.
    std::thread::sleep(Duration::from_millis(300));

    append(
        &path,
        &record(PROJECT, SESSION, "user", 2, "Live question."),
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut updated = false;
    while !updated && Instant::now() < deadline {
        if let Ok(report) = reports.recv_timeout(Duration::from_millis(500)) {
            updated = report
                .changes
                .iter()
                .any(|c| c.node_id == node_id && c.op == ChangeOp::Update);
        }
    }
    assert!(updated, "the watcher did not update the node");
    watcher.stop();
    let body = space
        .lock()
        .unwrap()
        .get_node(&node_id)
        .unwrap()
        .unwrap()
        .body;
    assert!(body.contains("Live question."));
}
