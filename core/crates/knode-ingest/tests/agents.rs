//! Gemini CLI, Copilot CLI, OpenCode, Amp and Aider: discovery, reading and capture, against
//! synthetic session files.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use knode_format::{Space, kinds, property_keys};
use knode_ingest::{
    Agent, Capture, Conversation, ProjectFilter, Role, SessionReader, TitleSource, discover,
    read_session, read_sessions, to_markdown,
};
use rusqlite::{Connection, params};
use tempfile::TempDir;

const PROJECT: &str = "/Users/example/project";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tools(conversation: &Conversation, message: usize) -> Vec<(String, String, bool)> {
    conversation.messages[message]
        .tool_calls()
        .map(|c| (c.name.clone(), c.summary.clone(), c.failed))
        .collect()
}

fn tool(name: &str, summary: &str, failed: bool) -> (String, String, bool) {
    (name.to_string(), summary.to_string(), failed)
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

fn append(path: &Path, text: &str) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

// Gemini CLI

#[test]
fn discovers_gemini_sessions_but_not_subagents() {
    let files = discover(Agent::GeminiCli, &fixtures().join("gemini/tmp")).unwrap();
    let names: Vec<String> = files
        .iter()
        .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        [
            "session-2025-06-01T08-00-cccc3333.json",
            "session-2026-01-02T10-00-aaaa1111.jsonl"
        ]
    );
}

#[test]
fn reads_a_gemini_session_with_rewrites_and_rewinds() {
    let file = discover(Agent::GeminiCli, &fixtures().join("gemini/tmp"))
        .unwrap()
        .pop()
        .unwrap();
    let conversation = read_session(&file).unwrap();

    assert_eq!(
        conversation.session_id,
        "aaaa1111-1111-4222-8333-444455556666"
    );
    assert_eq!(conversation.project.as_deref(), Some(Path::new(PROJECT)));
    assert_eq!(conversation.model.as_deref(), Some("gemini-example-2"));
    let title = conversation.recorded_title.as_ref().unwrap();
    assert_eq!(
        (title.text.as_str(), title.source),
        ("Speed up the slow test", TitleSource::Agent)
    );
    assert_eq!(
        conversation.started_at.unwrap().to_rfc3339(),
        "2026-01-02T10:00:00+00:00"
    );
    assert_eq!(
        conversation.ended_at.unwrap().to_rfc3339(),
        "2026-01-02T10:00:45+00:00"
    );
    // The reply written three times is kept once, in its last form, with the bookkeeping
    // `update_topic` call left out.
    assert_eq!(
        tools(&conversation, 1),
        [
            tool("run_shell_command", "cargo test --release", true),
            tool("read_file", "/Users/example/project/tests/slow.rs", false),
        ]
    );
    let markdown = to_markdown(&conversation);
    assert_eq!(
        markdown,
        include_str!("fixtures/gemini/expected.md"),
        "rendered:\n{markdown}"
    );
    assert!(!markdown.contains("never kept"));
    assert!(!markdown.contains("Wrong turn"));
}

#[test]
fn reads_an_older_whole_file_gemini_session() {
    let file = discover(Agent::GeminiCli, &fixtures().join("gemini/tmp"))
        .unwrap()
        .remove(0);
    let conversation = read_session(&file).unwrap();
    assert_eq!(
        conversation.session_id,
        "cccc3333-1111-4222-8333-444455556666"
    );
    // Older versions named the folder after a hash of the project, with no `.project_root`.
    assert_eq!(conversation.project, None);
    assert_eq!(conversation.title(), "List the files");
    assert_eq!(conversation.messages.len(), 2);
    assert_eq!(
        tools(&conversation, 1),
        [tool("list_directory", ".", false)]
    );
    assert_eq!(conversation.messages[1].text(), "Two files.");
}

#[test]
fn follows_a_growing_gemini_session_and_its_rewrites() {
    let dir = TempDir::new().unwrap();
    let chats = dir.path().join("tmp/demo/chats");
    fs::create_dir_all(&chats).unwrap();
    let path = chats.join("session-2026-01-02T10-00-eeee5555.jsonl");
    fs::write(
        &path,
        "{\"sessionId\":\"eeee5555\",\"projectHash\":\"0\",\"startTime\":\"2026-01-02T10:00:00Z\",\"lastUpdated\":\"2026-01-02T10:00:00Z\",\"kind\":\"main\"}\n\
         {\"id\":\"u\",\"timestamp\":\"2026-01-02T10:00:01Z\",\"type\":\"user\",\"content\":\"Hi\"}\n\
         {\"id\":\"g\",\"timestamp\":\"2026-01-02T10:00:02Z\",\"type\":\"gemini\",\"content\":\"Hel\"}\n",
    )
    .unwrap();
    let mut reader = SessionReader::open(Agent::GeminiCli, &path, "file-name-id");
    reader.poll().unwrap();
    assert_eq!(reader.conversation().messages[1].text(), "Hel");

    // Continue from saved state, as capture does after a restart.
    let saved = serde_json::to_string(reader.conversation()).unwrap();
    let mut reader = SessionReader::resume(
        &path,
        serde_json::from_str(&saved).unwrap(),
        reader.offset(),
    );
    append(
        &path,
        "{\"id\":\"g\",\"timestamp\":\"2026-01-02T10:00:02Z\",\"type\":\"gemini\",\"content\":\"Hello there\"}\n",
    );
    let progress = reader.poll().unwrap();
    assert_eq!(progress.records, 1);
    assert_eq!(reader.conversation().messages.len(), 2);
    assert_eq!(reader.conversation().messages[1].text(), "Hello there");
}

// Copilot CLI

#[test]
fn reads_a_copilot_cli_session() {
    let root = fixtures().join("copilot/session-state");
    let files = discover(Agent::CopilotCli, &root).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].session_id, "c0c0c0c0-1111-4222-8333-444455556666");
    let conversation = read_session(&files[0]).unwrap();

    assert_eq!(conversation.project.as_deref(), Some(Path::new(PROJECT)));
    assert_eq!(conversation.git_branch.as_deref(), Some("main"));
    assert_eq!(conversation.model.as_deref(), Some("model-example-2"));
    assert_eq!(conversation.title(), "Add a README");
    assert_eq!(
        conversation.ended_at.unwrap().to_rfc3339(),
        "2026-01-02T12:00:11+00:00"
    );
    let roles: Vec<Role> = conversation.messages.iter().map(|m| m.role).collect();
    assert_eq!(roles, [Role::User, Role::Assistant]);
    // What the person typed, not the context the CLI wrapped it in.
    assert_eq!(conversation.messages[0].text(), "Add a README");
    assert_eq!(
        tools(&conversation, 1),
        [
            tool("bash", "ls -a", false),
            tool("create", "/Users/example/project/README.md", true),
        ]
    );
    let markdown = to_markdown(&conversation);
    assert!(!markdown.contains("Subagent notes"));
    assert!(!markdown.contains("never kept"));
    assert!(markdown.ends_with("Writing the file was denied.\n"));
}

// Amp

#[test]
fn reads_an_amp_thread() {
    let files = discover(Agent::Amp, &fixtures().join("amp/threads")).unwrap();
    assert_eq!(files.len(), 1);
    let conversation = read_session(&files[0]).unwrap();

    assert_eq!(
        conversation.session_id,
        "T-dddd4444-1111-4222-8333-444455556666"
    );
    assert_eq!(
        conversation.project.as_deref(),
        Some(Path::new("/Users/example/my project"))
    );
    assert_eq!(conversation.title(), "Rename the config module");
    assert_eq!(conversation.model.as_deref(), Some("amp-example-model"));
    assert_eq!(
        conversation.started_at.unwrap().to_rfc3339(),
        "2026-01-02T13:00:00+00:00"
    );
    let roles: Vec<Role> = conversation.messages.iter().map(|m| m.role).collect();
    assert_eq!(roles, [Role::User, Role::Assistant]);
    assert_eq!(
        tools(&conversation, 1),
        [
            tool("Bash", "git mv src/config.rs src/settings.rs", false),
            tool("edit_file", "/Users/example/my project/src/lib.rs", true),
        ]
    );
    assert_eq!(
        conversation.messages[1].text(),
        "Renaming it.\n\nRenamed; one edit failed."
    );
    assert!(!to_markdown(&conversation).contains("never kept"));
}

// Aider

#[test]
fn reads_each_aider_session_as_a_conversation() {
    let root = fixtures().join("aider");
    let files = discover(Agent::Aider, &root).unwrap();
    // Hidden folders are not searched.
    assert_eq!(files.len(), 1);
    let project = root.join("project-a");
    assert_eq!(files[0].path, project.join(".aider.chat.history.md"));

    let conversations = read_sessions(&files[0]).unwrap();
    assert_eq!(conversations.len(), 2);
    let first = &conversations[0];
    assert!(first.session_id.starts_with("aider-"));
    assert!(first.session_id.ends_with("-20260102T090000"));
    assert_eq!(first.project.as_deref(), Some(project.as_path()));
    assert_eq!(first.title(), "Add a hello function");
    let roles: Vec<Role> = first.messages.iter().map(|m| m.role).collect();
    use Role::{Assistant, User};
    assert_eq!(roles, [User, Assistant, User, Assistant]);
    assert_eq!(
        first.messages[0].text(),
        "Add a hello function\nthat prints Hello"
    );
    assert_eq!(
        tools(first, 1),
        [
            tool("edit", "hello.py", false),
            tool("commit", "1a2b3c4 feat: add hello function", false),
        ]
    );
    assert_eq!(first.messages[2].text(), "/run python hello.py");
    assert_eq!(tools(first, 3), [tool("run", "python hello.py", false)]);
    let markdown = to_markdown(first);
    assert_eq!(
        markdown,
        include_str!("fixtures/aider/expected.md"),
        "rendered:\n{markdown}"
    );

    let second = &conversations[1];
    assert!(second.session_id.ends_with("-20260103T143000"));
    assert_eq!(second.title(), "Explain hello.py");
    assert_eq!(
        second.messages[1].text(),
        "It defines `hello`, which prints a greeting."
    );
    // `read_session` gives the newest.
    assert_eq!(
        read_session(&files[0]).unwrap().session_id,
        second.session_id
    );
}

#[test]
fn excluded_aider_projects_are_skipped_before_reading() {
    let root = fixtures().join("aider");
    let file = discover(Agent::Aider, &root).unwrap().remove(0);
    assert!(!ProjectFilter::excluding([root.join("project-a")]).may_allow_file(&file));
    assert!(ProjectFilter::excluding([root.join("project-b")]).may_allow_file(&file));
}

// OpenCode

/// A synthetic OpenCode database with the tables and columns the reader uses.
struct OpenCodeDb {
    root: PathBuf,
    connection: Connection,
}

impl OpenCodeDb {
    fn create(root: &Path) -> Self {
        fs::create_dir_all(root).unwrap();
        let connection = Connection::open(root.join("opencode.db")).unwrap();
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE session (id text PRIMARY KEY, project_id text NOT NULL,
                   parent_id text, slug text NOT NULL, directory text NOT NULL,
                   title text NOT NULL, version text NOT NULL, time_created integer NOT NULL,
                   time_updated integer NOT NULL);
                 CREATE TABLE message (id text PRIMARY KEY, session_id text NOT NULL,
                   time_created integer NOT NULL, time_updated integer NOT NULL,
                   data text NOT NULL);
                 CREATE TABLE part (id text PRIMARY KEY, message_id text NOT NULL,
                   session_id text NOT NULL, time_created integer NOT NULL,
                   time_updated integer NOT NULL, data text NOT NULL);",
            )
            .unwrap();
        Self {
            root: root.to_path_buf(),
            connection,
        }
    }

    fn session(&self, id: &str, parent: Option<&str>, title: &str, updated: i64) {
        self.connection
            .execute(
                "INSERT OR REPLACE INTO session VALUES (?1, 'p', ?2, 's', ?3, ?4, '1', ?5, ?6)",
                params![id, parent, PROJECT, title, 1_767_348_000_000_i64, updated],
            )
            .unwrap();
    }

    fn message(&self, session: &str, id: &str, at: i64, data: serde_json::Value) {
        self.connection
            .execute(
                "INSERT OR REPLACE INTO message VALUES (?1, ?2, ?3, ?3, ?4)",
                params![id, session, at, data.to_string()],
            )
            .unwrap();
    }

    fn part(&self, session: &str, message: &str, id: &str, data: serde_json::Value) {
        self.connection
            .execute(
                "INSERT OR REPLACE INTO part VALUES (?1, ?2, ?3, 0, 0, ?4)",
                params![id, message, session, data.to_string()],
            )
            .unwrap();
    }

    fn fill(&self) {
        use serde_json::json;
        let s = "ses_01";
        self.session(s, None, "Fix the flaky test", 1_767_348_060_000);
        self.session("ses_02", Some(s), "Child session - x", 1_767_348_030_000);
        self.message(
            s,
            "msg_01",
            1_767_348_001_000,
            json!({"role": "user", "time": {"created": 1_767_348_001_000_i64}}),
        );
        self.part(
            s,
            "msg_01",
            "prt_01",
            json!({"type": "text", "text": "Fix the flaky test"}),
        );
        self.part(
            s,
            "msg_01",
            "prt_02",
            json!({"type": "text", "text": "Contents of a file OpenCode added", "synthetic": true}),
        );
        self.message(
            s,
            "msg_02",
            1_767_348_002_000,
            json!({"role": "assistant", "modelID": "model-example", "path": {"cwd": PROJECT, "root": PROJECT}}),
        );
        self.part(s, "msg_02", "prt_03", json!({"type": "step-start"}));
        self.part(
            s,
            "msg_02",
            "prt_04",
            json!({"type": "reasoning", "text": "Private reasoning"}),
        );
        self.part(
            s,
            "msg_02",
            "prt_05",
            json!({"type": "tool", "callID": "call_1", "tool": "bash",
                   "state": {"status": "completed", "input": {"command": "cargo test", "description": "Run tests"},
                             "output": "output that is never kept", "title": "Run tests"}}),
        );
        self.part(
            s,
            "msg_02",
            "prt_06",
            json!({"type": "tool", "callID": "call_2", "tool": "edit",
                   "state": {"status": "error", "input": {"filePath": "/Users/example/project/src/lib.rs"}, "error": "no match"}}),
        );
        self.part(
            s,
            "msg_02",
            "prt_07",
            json!({"type": "text", "text": "The test waits on a timer."}),
        );
        self.part(
            s,
            "msg_02",
            "prt_08",
            json!({"type": "step-finish", "reason": "stop"}),
        );
    }
}

#[test]
fn reads_opencode_sessions_from_the_database() {
    let dir = TempDir::new().unwrap();
    let db = OpenCodeDb::create(&dir.path().join("opencode"));
    db.fill();

    let files = discover(Agent::OpenCode, &db.root).unwrap();
    // Child sessions belong to subagents and are left out.
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].session_id, "ses_01");
    assert_eq!(files[0].path, db.root.join("opencode.db#ses_01"));
    assert_eq!(files[0].len, 1_767_348_060_000);

    let mut reader = SessionReader::new(&files[0]);
    reader.poll().unwrap();
    let conversation = reader.conversation();
    assert_eq!(conversation.project.as_deref(), Some(Path::new(PROJECT)));
    assert_eq!(conversation.model.as_deref(), Some("model-example"));
    assert_eq!(conversation.title(), "Fix the flaky test");
    assert_eq!(conversation.messages[0].text(), "Fix the flaky test");
    assert_eq!(
        tools(conversation, 1),
        [
            tool("bash", "cargo test", false),
            tool("edit", "/Users/example/project/src/lib.rs", true),
        ]
    );
    assert_eq!(
        conversation.messages[1].text(),
        "The test waits on a timer."
    );
    let markdown = to_markdown(conversation);
    assert!(!markdown.contains("never kept") && !markdown.contains("Private reasoning"));
    assert!(!markdown.contains("OpenCode added"));

    // Nothing changed: nothing is read.
    assert_eq!(reader.poll().unwrap().records, 0);
    // A new turn moves `time_updated`, and the session is read again.
    db.message(
        "ses_01",
        "msg_03",
        1_767_348_070_000,
        serde_json::json!({"role": "user"}),
    );
    db.part(
        "ses_01",
        "msg_03",
        "prt_09",
        serde_json::json!({"type": "text", "text": "Thanks"}),
    );
    db.session("ses_01", None, "Fix the flaky test", 1_767_348_070_000);
    assert_eq!(reader.poll().unwrap().records, 1);
    assert_eq!(reader.conversation().messages.len(), 3);
}

#[test]
fn a_missing_opencode_database_has_no_sessions() {
    let dir = TempDir::new().unwrap();
    assert!(discover(Agent::OpenCode, dir.path()).unwrap().is_empty());
}

// Capture

#[test]
fn captures_every_agent_into_a_space() {
    let dir = TempDir::new().unwrap();
    let mut space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
    let gemini = dir.path().join("gemini");
    let copilot = dir.path().join("copilot");
    let amp = dir.path().join("amp");
    let aider = dir.path().join("aider");
    copy_dir(&fixtures().join("gemini/tmp"), &gemini);
    copy_dir(&fixtures().join("copilot/session-state"), &copilot);
    copy_dir(&fixtures().join("amp/threads"), &amp);
    copy_dir(&fixtures().join("aider"), &aider);
    let db = OpenCodeDb::create(&dir.path().join("opencode"));
    db.fill();

    let mut capture = Capture::open(dir.path().join("state.json"))
        .unwrap()
        .with_roots([
            (Agent::GeminiCli, gemini.clone()),
            (Agent::CopilotCli, copilot),
            (Agent::OpenCode, db.root.clone()),
            (Agent::Amp, amp),
            (Agent::Aider, aider.clone()),
        ]);
    let report = capture.sync_all(&mut space).unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    let mut agents: Vec<&str> = report.created().map(|c| c.agent.id()).collect();
    agents.sort_unstable();
    assert_eq!(
        agents,
        [
            "aider",
            "aider",
            "amp",
            "copilot-cli",
            "gemini-cli",
            "gemini-cli",
            "opencode"
        ]
    );
    for change in report.created() {
        let node = space.get_node(&change.node_id).unwrap().unwrap();
        assert_eq!(node.kind, kinds::CONVERSATION);
        assert_eq!(
            space
                .property(&change.node_id, property_keys::CONVERSATION_AGENT)
                .unwrap()
                .as_deref(),
            Some(change.agent.id())
        );
    }

    // Nothing new: nothing written.
    let again = capture.sync_all(&mut space).unwrap();
    assert!(again.changes.is_empty(), "{again:?}");

    // Aider starts another session in the same file: a new node, the others stay.
    let history = aider.join("project-a/.aider.chat.history.md");
    append(
        &history,
        "\n# aider chat started at 2026-01-05 08:00:00\n\n#### Add tests  \n\nAdded them.\n",
    );
    let report = capture.sync_paths(&mut space, &[&history]).unwrap();
    assert_eq!(report.created().count(), 1, "{report:?}");
    let created = report.created().next().unwrap();
    assert!(created.session_id.ends_with("-20260105T080000"));
    let node = space.get_node(&created.node_id).unwrap().unwrap();
    assert_eq!(node.title, "Add tests");

    // A change to the OpenCode database reads only the sessions that changed.
    db.session(
        "ses_01",
        None,
        "Fix the flaky timer test",
        1_767_348_090_000,
    );
    let report = capture
        .sync_paths(&mut space, &[db.root.join("opencode.db-wal")])
        .unwrap();
    assert_eq!(report.updated().count(), 1, "{report:?}");
    let updated = report.updated().next().unwrap();
    assert_eq!(updated.agent, Agent::OpenCode);
    assert_eq!(
        space.get_node(&updated.node_id).unwrap().unwrap().title,
        "Fix the flaky timer test"
    );

    // The Gemini session folder is listed by the capture roots.
    assert!(capture.roots().iter().any(|(_, root)| root == &gemini));
}
