//! Cursor, Windsurf, Cline and Roo Code: discovery, reading and capture, against synthetic
//! editor databases and task folders built here.

use std::fs;
use std::path::{Path, PathBuf};

use knode_format::{Space, kinds, property_keys};
use knode_ingest::{
    Agent, Capture, Conversation, Role, SessionReader, TitleSource, discover, read_session,
    to_markdown,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tempfile::TempDir;

const PROJECT: &str = "/Users/example/project";

fn tools(conversation: &Conversation, message: usize) -> Vec<(String, String, bool)> {
    conversation.messages[message]
        .tool_calls()
        .map(|c| (c.name.clone(), c.summary.clone(), c.failed))
        .collect()
}

fn tool(name: &str, summary: &str, failed: bool) -> (String, String, bool) {
    (name.to_string(), summary.to_string(), failed)
}

/// Creates a VS Code-style state database in WAL mode, as the editors keep it, and keeps the
/// connection open so its writes stay in the WAL like a running editor's.
fn state_db(dir: &Path) -> Connection {
    fs::create_dir_all(dir).unwrap();
    let connection = Connection::open(dir.join("state.vscdb")).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
             CREATE TABLE cursorDiskKV (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);",
        )
        .unwrap();
    connection
}

fn put(connection: &Connection, table: &str, key: &str, value: &Value) {
    connection
        .execute(
            &format!("INSERT INTO {table} (key, value) VALUES (?1, ?2)"),
            params![key, value.to_string()],
        )
        .unwrap();
}

// Cursor

/// A synthetic Cursor `User` folder: the global database and one workspace folder.
struct CursorUser {
    root: PathBuf,
    connection: Connection,
}

impl CursorUser {
    fn create(user: &Path) -> Self {
        let root = user.join("globalStorage");
        let connection = state_db(&root);
        connection
            .execute_batch(
                "CREATE TABLE composerHeaders (composerId TEXT PRIMARY KEY, workspaceId TEXT,
                   createdAt INTEGER, lastUpdatedAt INTEGER, isArchived INTEGER,
                   isSubagent INTEGER, recency INTEGER, checkpointAt INTEGER, value TEXT,
                   subagentTypeName TEXT);",
            )
            .unwrap();
        let workspace = user.join("workspaceStorage/0a1b2c3d");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(
            workspace.join("workspace.json"),
            json!({"folder": "file:///Users/example/other%20app"}).to_string(),
        )
        .unwrap();
        Self { root, connection }
    }

    fn composer(&self, id: &str, data: Value) {
        put(
            &self.connection,
            "cursorDiskKV",
            &format!("composerData:{id}"),
            &data,
        );
    }

    fn bubble(&self, composer: &str, id: &str, data: Value) {
        put(
            &self.connection,
            "cursorDiskKV",
            &format!("bubbleId:{composer}:{id}"),
            &data,
        );
    }

    fn header(&self, id: &str, workspace: &str, subagent: bool) {
        self.connection
            .execute(
                "INSERT OR REPLACE INTO composerHeaders (composerId, workspaceId, isSubagent, value)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    id,
                    workspace,
                    i64::from(subagent),
                    json!({"composerId": id, "workspaceIdentifier": {"id": workspace}}).to_string()
                ],
            )
            .unwrap();
    }

    fn fill(&self) {
        let headers = json!([
            {"bubbleId": "b1", "type": 1},
            {"bubbleId": "b2", "type": 2},
            {"bubbleId": "b3", "type": 2},
            {"bubbleId": "b4", "type": 2},
            {"bubbleId": "b5", "type": 2},
            {"bubbleId": "missing", "type": 2}
        ]);
        self.composer(
            "c-agent",
            json!({
                "_v": 16, "composerId": "c-agent", "name": "Fix the login form",
                "createdAt": 1_767_348_000_000_i64, "lastUpdatedAt": 1_767_348_090_000_i64,
                "unifiedMode": "agent", "status": "completed",
                "modelConfig": {"modelName": "model-example"},
                "workspaceIdentifier": {"id": "w1", "uri": {"fsPath": PROJECT, "external": "file:///Users/example/project"}},
                "trackedGitRepos": [{"repoPath": PROJECT, "branches": [
                    {"branchName": "main", "lastInteractionAt": 1},
                    {"branchName": "fix/login", "lastInteractionAt": 2}
                ]}],
                "fullConversationHeadersOnly": headers,
                "conversationMap": {}
            }),
        );
        self.bubble(
            "c-agent",
            "b1",
            json!({"_v": 3, "type": 1, "bubbleId": "b1", "text": "The login form loses the email",
                   "createdAt": "2026-01-02T10:00:00.000Z", "images": [{"path": "/tmp/shot.png"}]}),
        );
        self.bubble(
            "c-agent",
            "b2",
            json!({"_v": 3, "type": 2, "bubbleId": "b2", "text": "",
                   "thinking": {"text": "Private reasoning"},
                   "toolFormerData": {"toolCallId": "t1", "name": "read_file", "status": "completed",
                                      "rawArgs": "{\"target_file\": \"src/login.tsx\"}",
                                      "result": "file contents that are never kept"}}),
        );
        self.bubble(
            "c-agent",
            "b3",
            json!({"_v": 3, "type": 2, "bubbleId": "b3", "text": "",
                   "toolFormerData": {"toolCallId": "t2", "name": "run_terminal_cmd", "status": "error",
                                      "rawArgs": "{\"command\": \"npm test\", \"is_background\": false}"}}),
        );
        self.bubble(
            "c-agent",
            "b4",
            json!({"_v": 3, "type": 2, "bubbleId": "b4", "text": "The form resets on submit.",
                   "createdAt": "2026-01-02T10:01:00.000Z", "modelInfo": {"modelName": "model-newer"}}),
        );
        // A bubble with nothing to show is left out.
        self.bubble("c-agent", "b5", json!({"_v": 3, "type": 2, "text": "  "}));

        // An older composer with its turns inline, and its project only in workspaceStorage.
        self.composer(
            "c-chat",
            json!({
                "_v": 2, "composerId": "c-chat", "createdAt": 1_767_000_000_000_i64,
                "conversation": [
                    {"type": 1, "bubbleId": "i1", "text": "What does this regex match?"},
                    {"type": 2, "bubbleId": "i2", "text": "Dates in ISO format."}
                ]
            }),
        );
        self.header("c-chat", "0a1b2c3d", false);

        // Left out: no turns, a subagent, a best-of-N subcomposer, a value that is not JSON.
        self.composer(
            "c-empty",
            json!({"_v": 16, "composerId": "c-empty", "fullConversationHeadersOnly": []}),
        );
        self.composer("c-sub", json!({"composerId": "c-sub", "fullConversationHeadersOnly": [{"bubbleId": "s1", "type": 1}]}));
        self.header("c-sub", "w1", true);
        self.composer("c-best", json!({"composerId": "c-best", "isBestOfNSubcomposer": true,
                                        "fullConversationHeadersOnly": [{"bubbleId": "x", "type": 1}]}));
        self.connection
            .execute(
                "INSERT INTO cursorDiskKV VALUES ('composerData:c-broken', 'not json')",
                [],
            )
            .unwrap();
        put(
            &self.connection,
            "ItemTable",
            "composer.composerHeaders",
            &json!({"allComposers": []}),
        );
    }
}

#[test]
fn reads_cursor_conversations_from_the_global_database() {
    let dir = TempDir::new().unwrap();
    let cursor = CursorUser::create(&dir.path().join("User"));
    cursor.fill();

    let files = discover(Agent::Cursor, &cursor.root).unwrap();
    let ids: Vec<&str> = files.iter().map(|f| f.session_id.as_str()).collect();
    assert_eq!(ids, ["c-agent", "c-chat"]);
    assert_eq!(files[0].path, cursor.root.join("state.vscdb#c-agent"));
    assert!(files[0].modified.is_some());

    let mut reader = SessionReader::new(&files[0]);
    reader.poll().unwrap();
    let conversation = reader.conversation();
    assert_eq!(conversation.session_id, "c-agent");
    assert_eq!(conversation.project.as_deref(), Some(Path::new(PROJECT)));
    assert_eq!(conversation.git_branch.as_deref(), Some("fix/login"));
    assert_eq!(conversation.model.as_deref(), Some("model-newer"));
    assert_eq!(conversation.title(), "Fix the login form");
    assert_eq!(
        conversation.recorded_title.as_ref().unwrap().source,
        TitleSource::Agent
    );
    assert_eq!(conversation.messages.len(), 2);
    assert_eq!(conversation.messages[0].role, Role::User);
    assert_eq!(
        conversation.messages[0].text(),
        "The login form loses the email"
    );
    assert_eq!(
        tools(conversation, 1),
        [
            tool("read_file", "src/login.tsx", false),
            tool("run_terminal_cmd", "npm test", true),
        ]
    );
    assert_eq!(
        conversation.messages[1].text(),
        "The form resets on submit."
    );
    let markdown = to_markdown(conversation);
    assert!(!markdown.contains("never kept") && !markdown.contains("Private reasoning"));
    assert_eq!(
        conversation.started_at.unwrap().to_rfc3339(),
        "2026-01-02T10:00:00+00:00"
    );

    // Nothing changed: nothing is read.
    assert_eq!(reader.poll().unwrap().records, 0);
    // A new turn rewrites the composer, and the conversation is read again.
    cursor.bubble("c-agent", "b6", json!({"type": 1, "text": "Thanks"}));
    let mut data: Value = serde_json::from_str(
        &cursor
            .connection
            .query_row(
                "SELECT value FROM cursorDiskKV WHERE key = 'composerData:c-agent'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
    )
    .unwrap();
    data["fullConversationHeadersOnly"]
        .as_array_mut()
        .unwrap()
        .push(json!({"bubbleId": "b6", "type": 1}));
    cursor.composer("c-agent", data);
    assert_eq!(reader.poll().unwrap().records, 1);
    assert_eq!(reader.conversation().messages.len(), 3);

    // An older composer: turns inline, project from its workspace folder, title derived.
    let chat = read_session(&files[1]).unwrap();
    assert_eq!(
        chat.project.as_deref(),
        Some(Path::new("/Users/example/other app"))
    );
    assert!(chat.recorded_title.is_none());
    assert_eq!(chat.title(), "What does this regex match?");
    assert_eq!(chat.messages.len(), 2);

    // A deleted composer is gone.
    cursor
        .connection
        .execute(
            "DELETE FROM cursorDiskKV WHERE key = 'composerData:c-agent'",
            [],
        )
        .unwrap();
    assert!(reader.poll().is_err());
}

#[test]
fn reads_editor_databases_without_writing_or_blocking_them() {
    let dir = TempDir::new().unwrap();
    let cursor = CursorUser::create(&dir.path().join("User"));
    cursor.fill();
    let database = cursor.root.join("state.vscdb");
    // The editor's writes are still in the WAL: reading through it finds them.
    assert!(cursor.root.join("state.vscdb-wal").exists());
    let before = fs::read(&database).unwrap();

    // The editor is in the middle of a write while capture reads.
    cursor.connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    cursor.bubble("c-agent", "b9", json!({"type": 1, "text": "uncommitted"}));
    let files = discover(Agent::Cursor, &cursor.root).unwrap();
    assert_eq!(files.len(), 2);
    let conversation = read_session(&files[0]).unwrap();
    assert!(!to_markdown(&conversation).contains("uncommitted"));
    // The editor can still commit: the reader held no lock against it.
    cursor.connection.execute_batch("COMMIT").unwrap();

    // Reading wrote nothing to the database file (the editor's commit went to the WAL).
    assert_eq!(fs::read(&database).unwrap(), before);

    // With no WAL (a closed database), it is read without any lock.
    drop(cursor);
    assert!(
        !dir.path()
            .join("User/globalStorage/state.vscdb-wal")
            .exists()
    );
    let files = discover(Agent::Cursor, &dir.path().join("User/globalStorage")).unwrap();
    assert_eq!(files.len(), 2);
    let writer = Connection::open(&database).unwrap();
    writer
        .execute_batch("BEGIN IMMEDIATE; DELETE FROM ItemTable; COMMIT;")
        .unwrap();
}

#[test]
fn a_missing_or_foreign_editor_database_has_no_conversations() {
    let dir = TempDir::new().unwrap();
    assert!(discover(Agent::Cursor, dir.path()).unwrap().is_empty());
    assert!(discover(Agent::Windsurf, dir.path()).unwrap().is_empty());
    // A state database without Cursor's table.
    let connection = Connection::open(dir.path().join("state.vscdb")).unwrap();
    connection
        .execute_batch("CREATE TABLE ItemTable (key TEXT, value BLOB);")
        .unwrap();
    assert!(discover(Agent::Cursor, dir.path()).unwrap().is_empty());
}

// Windsurf

fn windsurf_workspace(root: &Path, name: &str, folder: &str, tabs: Value) -> Connection {
    let dir = root.join(name);
    let connection = state_db(&dir);
    fs::write(
        dir.join("workspace.json"),
        json!({"folder": folder}).to_string(),
    )
    .unwrap();
    put(
        &connection,
        "ItemTable",
        "workbench.panel.aichat.view.aichat.chatdata",
        &json!({"tabs": tabs}),
    );
    connection
}

#[test]
fn reads_windsurf_chats_from_each_workspace() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("workspaceStorage");
    let connection = windsurf_workspace(
        &root,
        "aaaa",
        "file:///Users/example/project",
        json!([
            {"tabId": "tab-1", "chatTitle": "Explain the cache", "lastSendTime": 1_767_348_000_000_i64,
             "bubbles": [
                {"type": "user", "id": "u1", "text": "How does the cache expire?"},
                {"type": "ai", "id": "a1", "rawText": "After ten minutes."},
                {"type": "other", "text": "ignored"}
             ]},
            {"tabId": "tab-empty", "bubbles": []}
        ]),
    );
    fs::create_dir_all(root.join("no-database")).unwrap();

    let files = discover(Agent::Windsurf, &root).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, root.join("aaaa/state.vscdb#tab-1"));
    let mut reader = SessionReader::new(&files[0]);
    reader.poll().unwrap();
    let conversation = reader.conversation();
    assert_eq!(conversation.project.as_deref(), Some(Path::new(PROJECT)));
    assert_eq!(conversation.title(), "Explain the cache");
    assert_eq!(conversation.messages.len(), 2);
    assert_eq!(conversation.messages[1].text(), "After ten minutes.");
    assert_eq!(reader.poll().unwrap().records, 0);

    put(
        &connection,
        "ItemTable",
        "workbench.panel.aichat.view.aichat.chatdata",
        &json!({"tabs": [{"tabId": "tab-1", "chatTitle": "Explain the cache", "bubbles": [
            {"type": "user", "text": "How does the cache expire?"},
            {"type": "ai", "text": "After ten minutes."},
            {"type": "user", "text": "Thanks"}
        ]}]}),
    );
    assert_eq!(reader.poll().unwrap().records, 1);
    assert_eq!(reader.conversation().messages.len(), 3);
}

// Cline and Roo Code

fn task(root: &Path, id: &str, messages: Value) -> PathBuf {
    let dir = root.join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("ui_messages.json"), messages.to_string()).unwrap();
    dir
}

fn cline_messages() -> Value {
    json!([
        {"ts": 1_767_348_000_000_i64, "type": "say", "say": "task", "text": "Add a dark mode toggle", "images": ["data:image/png;base64,AAAA"]},
        {"ts": 1_767_348_001_000_i64, "type": "say", "say": "api_req_started", "text": "{\"request\":\"...\"}"},
        {"ts": 1_767_348_002_000_i64, "type": "say", "say": "reasoning", "text": "Private reasoning"},
        {"ts": 1_767_348_003_000_i64, "type": "say", "say": "text", "text": "I will look at the settings page first."},
        {"ts": 1_767_348_004_000_i64, "type": "ask", "ask": "tool", "text": "{\"tool\":\"readFile\",\"path\":\"src/settings.tsx\",\"content\":\"file contents that are never kept\"}"},
        {"ts": 1_767_348_005_000_i64, "type": "ask", "ask": "tool", "text": "{\"tool\":\"editedExistingFile\",\"path\":\"src/theme.ts\",\"diff\":\"...\"}"},
        {"ts": 1_767_348_006_000_i64, "type": "say", "say": "diff_error", "text": "src/theme.ts"},
        {"ts": 1_767_348_007_000_i64, "type": "ask", "ask": "command", "text": "npm run lint"},
        {"ts": 1_767_348_008_000_i64, "type": "say", "say": "command_output", "text": "output that is never kept"},
        {"ts": 1_767_348_009_000_i64, "type": "ask", "ask": "use_mcp_server", "text": "{\"type\":\"use_mcp_tool\",\"serverName\":\"knode\",\"toolName\":\"search\",\"arguments\":\"{}\"}"},
        {"ts": 1_767_348_010_000_i64, "type": "ask", "ask": "followup", "text": "{\"question\":\"Should it follow the system setting?\",\"options\":[\"Yes\",\"No\"]}"},
        {"ts": 1_767_348_011_000_i64, "type": "say", "say": "user_feedback", "text": "Yes", "images": []},
        {"ts": 1_767_348_012_000_i64, "type": "say", "say": "completion_result", "text": "The toggle follows the system setting."},
        {"ts": 1_767_348_013_000_i64, "type": "say", "say": "text", "text": "partial text", "partial": true}
    ])
}

#[test]
fn reads_a_cline_task() {
    let dir = TempDir::new().unwrap();
    let extension = dir.path().join("saoudrizwan.claude-dev");
    let root = extension.join("tasks");
    let task_dir = task(&root, "1767348000000", cline_messages());
    fs::write(
        task_dir.join("api_conversation_history.json"),
        json!([{"role": "user", "content": [{"type": "text", "text": format!(
            "<task>Add a dark mode toggle</task>\n<environment_details>\n# Current Working Directory ({PROJECT}) Files\nsrc/\n</environment_details>"
        )}]}])
        .to_string(),
    )
    .unwrap();
    fs::write(
        task_dir.join("task_metadata.json"),
        json!({"files_in_context": [], "model_usage": [
            {"ts": 1, "model_id": "model-old", "model_provider_id": "p", "mode": "act"},
            {"ts": 2, "model_id": "model-example", "model_provider_id": "p", "mode": "act"}
        ]})
        .to_string(),
    )
    .unwrap();
    // Not a task: a file in the tasks folder, and a folder without ui_messages.json.
    fs::write(root.join("notes.json"), "{}").unwrap();
    fs::create_dir_all(root.join("1767000000000")).unwrap();

    let files = discover(Agent::Cline, &root).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].session_id, "1767348000000");
    let conversation = read_session(&files[0]).unwrap();
    assert_eq!(conversation.project.as_deref(), Some(Path::new(PROJECT)));
    assert_eq!(conversation.model.as_deref(), Some("model-example"));
    assert_eq!(conversation.title(), "Add a dark mode toggle");
    let roles: Vec<Role> = conversation.messages.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        [Role::User, Role::Assistant, Role::User, Role::Assistant]
    );
    assert!(
        conversation.messages[0]
            .parts
            .contains(&knode_ingest::Part::Image)
    );
    assert_eq!(
        tools(&conversation, 1),
        [
            tool("readFile", "src/settings.tsx", false),
            tool("editedExistingFile", "src/theme.ts", true),
            tool("execute_command", "npm run lint", false),
            tool("search", "knode", false),
        ]
    );
    assert!(
        conversation.messages[1]
            .text()
            .ends_with("Should it follow the system setting?")
    );
    assert_eq!(
        conversation.messages[3].text(),
        "The toggle follows the system setting."
    );
    let markdown = to_markdown(&conversation);
    assert!(!markdown.contains("never kept"));
    assert!(!markdown.contains("Private reasoning") && !markdown.contains("partial text"));
    assert_eq!(
        conversation.ended_at.unwrap().to_rfc3339(),
        "2026-01-02T10:00:12+00:00"
    );
}

#[test]
fn finds_the_project_of_a_cline_task_in_its_task_history() {
    let dir = TempDir::new().unwrap();
    let extension = dir.path().join("saoudrizwan.claude-dev");
    let root = extension.join("tasks");
    task(&root, "1767348000000", cline_messages());
    fs::create_dir_all(extension.join("state")).unwrap();
    fs::write(
        extension.join("state/taskHistory.json"),
        json!([{"id": "1767348000000", "ts": 1, "task": "Add a dark mode toggle",
                "cwdOnTaskInitialization": "/Users/example/web"}])
        .to_string(),
    )
    .unwrap();
    let file = discover(Agent::Cline, &root).unwrap().remove(0);
    let conversation = read_session(&file).unwrap();
    assert_eq!(
        conversation.project.as_deref(),
        Some(Path::new("/Users/example/web"))
    );
}

#[test]
fn reads_a_roo_code_task() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("rooveterinaryinc.roo-cline/tasks");
    let task_dir = task(
        &root,
        "0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee",
        json!([
            {"ts": 1_767_348_000_000_i64, "type": "say", "say": "text", "text": "Rename the user model"},
            {"ts": 1_767_348_001_000_i64, "type": "say", "say": "tool", "text": "{\"tool\":\"searchFiles\",\"path\":\"src\",\"regex\":\"User\"}"},
            {"ts": 1_767_348_002_000_i64, "type": "say", "say": "completion_result", "text": "Renamed."}
        ]),
    );
    fs::write(
        task_dir.join("history_item.json"),
        json!({"id": "0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee", "workspace": PROJECT, "task": "Rename the user model"}).to_string(),
    )
    .unwrap();
    let file = discover(Agent::RooCode, &root).unwrap().remove(0);
    let conversation = read_session(&file).unwrap();
    assert_eq!(conversation.agent, Agent::RooCode);
    assert_eq!(conversation.project.as_deref(), Some(Path::new(PROJECT)));
    assert_eq!(conversation.messages[0].text(), "Rename the user model");
    assert_eq!(tools(&conversation, 1), [tool("searchFiles", "src", false)]);
    assert_eq!(conversation.messages[1].text(), "Renamed.");
}

// Capture

#[test]
fn captures_editor_agents_into_a_space() {
    let dir = TempDir::new().unwrap();
    let mut space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
    let cursor = CursorUser::create(&dir.path().join("Cursor/User"));
    cursor.fill();
    let windsurf = dir.path().join("Windsurf/User/workspaceStorage");
    let _windsurf_db = windsurf_workspace(
        &windsurf,
        "aaaa",
        "file:///Users/example/project",
        json!([{"tabId": "tab-1", "chatTitle": "Explain the cache",
                "bubbles": [{"type": "user", "text": "How?"}, {"type": "ai", "text": "Like this."}]}]),
    );
    let cline = dir.path().join("cline/tasks");
    let cline_task = task(&cline, "1767348000000", cline_messages());
    let roo = dir.path().join("roo/tasks");
    task(
        &roo,
        "r1",
        json!([{"ts": 1, "type": "say", "say": "text", "text": "Hello"},
               {"ts": 2, "type": "say", "say": "text", "text": "Hi"}]),
    );

    let mut capture = Capture::open(dir.path().join("state.json"))
        .unwrap()
        .with_roots([
            (Agent::Cursor, cursor.root.clone()),
            (Agent::Windsurf, windsurf.clone()),
            (Agent::Cline, cline.clone()),
            (Agent::RooCode, roo),
        ]);
    let report = capture.sync_all(&mut space).unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    let mut agents: Vec<&str> = report.created().map(|c| c.agent.id()).collect();
    agents.sort_unstable();
    assert_eq!(
        agents,
        ["cline", "cursor", "cursor", "roo-code", "windsurf"]
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

    // A write to Cursor's database reads only the conversations that changed.
    cursor.composer(
        "c-chat",
        json!({"composerId": "c-chat", "name": "Explain the date regex", "conversation": [
            {"type": 1, "text": "What does this regex match?"},
            {"type": 2, "text": "Dates in ISO format."}
        ]}),
    );
    let report = capture
        .sync_paths(&mut space, &[cursor.root.join("state.vscdb-wal")])
        .unwrap();
    assert_eq!(report.updated().count(), 1, "{report:?}");
    let updated = report.updated().next().unwrap();
    assert_eq!(updated.agent, Agent::Cursor);
    assert_eq!(
        space.get_node(&updated.node_id).unwrap().unwrap().title,
        "Explain the date regex"
    );

    // A Cline task that grows is read again.
    let mut messages = cline_messages();
    messages.as_array_mut().unwrap().push(
        json!({"ts": 1_767_348_020_000_i64, "type": "say", "say": "user_feedback", "text": "Also add a test"}),
    );
    fs::write(cline_task.join("ui_messages.json"), messages.to_string()).unwrap();
    let report = capture
        .sync_paths(&mut space, &[cline_task.join("ui_messages.json")])
        .unwrap();
    assert_eq!(report.updated().count(), 1, "{report:?}");

    // A Windsurf workspace database change is found from its WAL.
    let report = capture
        .sync_paths(&mut space, &[windsurf.join("aaaa/state.vscdb-wal")])
        .unwrap();
    assert!(report.changes.is_empty(), "{report:?}");
    assert_eq!(report.unchanged, 1);
}

// Cursor, current layout: turn headers in chunks

#[test]
fn reads_cursor_turn_headers_kept_in_chunks() {
    let dir = TempDir::new().unwrap();
    let cursor = CursorUser::create(&dir.path().join("User"));
    // The composer of a current Cursor: no inline headers, a header log, and a creation time
    // far from its turns.
    cursor.composer(
        "c-chunked",
        json!({
            "_v": 18, "composerId": "c-chunked", "name": "Add a health check",
            "createdAt": 1_700_000_000_000_i64, "lastUpdatedAt": 1_767_348_000_000_i64,
            "unifiedMode": "agent", "isAgentic": true, "conversationState": "~",
            "modelConfig": {"modelName": "model-example", "maxMode": false},
            "workspaceIdentifier": {"id": "w1", "uri": {"$mid": 1, "fsPath": PROJECT,
                "external": "file:///Users/example/project", "path": PROJECT, "scheme": "file"}},
            "fullConversationHeadersOnly": [],
            "headerLog": {"count": 3, "chunkCount": 2},
            "conversationMap": {}
        }),
    );
    // The chunks are read by their numbers, whatever they are: here one starts at 1, one is not
    // padded, and the last, past a gap, repeats a turn already listed.
    put(
        &cursor.connection,
        "cursorDiskKV",
        "composerConvHeaders:c-chunked:000001",
        &json!([{"bubbleId": "h1", "type": 1}, {"bubbleId": "h2", "type": 2}]),
    );
    put(
        &cursor.connection,
        "cursorDiskKV",
        "composerConvHeaders:c-chunked:3",
        &json!([{"bubbleId": "h3", "type": 2, "serverBubbleId": "s3"}]),
    );
    put(
        &cursor.connection,
        "cursorDiskKV",
        "composerConvHeaders:c-chunked:000010",
        &json!([{"bubbleId": "h3", "type": 2}]),
    );
    // Not chunks of this composer: another name after the composer id, and another composer.
    put(
        &cursor.connection,
        "cursorDiskKV",
        "composerConvHeaders:c-chunked:draft",
        &json!([{"bubbleId": "h-draft", "type": 1}]),
    );
    put(
        &cursor.connection,
        "cursorDiskKV",
        "composerConvHeaders:c-chunked-2:000000",
        &json!([{"bubbleId": "h-other", "type": 1}]),
    );
    cursor.bubble(
        "c-chunked",
        "h1",
        json!({"_v": 3, "type": 1, "bubbleId": "h1", "text": "Add a health check endpoint",
               "createdAt": "2026-01-02T10:00:00.000Z"}),
    );
    cursor.bubble(
        "c-chunked",
        "h2",
        json!({"_v": 3, "type": 2, "bubbleId": "h2", "text": "", "createdAt": "2026-01-02T10:00:05.000Z",
               "toolFormerData": {"toolCallId": "t1", "name": "edit_file_v2", "status": "completed",
                                  "params": {"relativeWorkspacePath": "src/health.rs"},
                                  "result": "edit output that is never kept"}}),
    );
    cursor.bubble(
        "c-chunked",
        "h3",
        json!({"_v": 3, "type": 2, "bubbleId": "h3", "text": "The endpoint answers on /health.",
               "createdAt": "2026-01-02T10:02:00.000Z"}),
    );
    for (bubble, text) in [
        ("h-draft", "a draft turn"),
        ("h-other", "another composer's turn"),
    ] {
        cursor.bubble("c-chunked", bubble, json!({"type": 1, "text": text}));
    }
    // A turn no header lists any more, left from a retry, is not read either.
    cursor.bubble(
        "c-chunked",
        "orphan",
        json!({"type": 2, "text": "an earlier answer that was retried"}),
    );

    let files = discover(Agent::Cursor, &cursor.root).unwrap();
    assert_eq!(files.len(), 1);
    let mut reader = SessionReader::new(&files[0]);
    reader.poll().unwrap();
    let conversation = reader.conversation();
    assert_eq!(conversation.title(), "Add a health check");
    assert_eq!(conversation.project.as_deref(), Some(Path::new(PROJECT)));
    assert_eq!(conversation.model.as_deref(), Some("model-example"));
    assert_eq!(conversation.messages.len(), 2);
    assert_eq!(
        conversation.messages[0].text(),
        "Add a health check endpoint"
    );
    assert_eq!(
        tools(conversation, 1),
        [tool("edit_file_v2", "src/health.rs", false)]
    );
    assert_eq!(
        conversation.messages[1].text(),
        "The endpoint answers on /health."
    );
    let markdown = to_markdown(conversation);
    assert!(!markdown.contains("never kept"));
    assert!(!markdown.contains("draft turn") && !markdown.contains("another composer"));
    assert!(!markdown.contains("retried"));
    // The start is the first turn, not the composer's creation time; the end is the last turn,
    // which the composer's last update lags behind.
    assert_eq!(
        conversation.started_at.unwrap().to_rfc3339(),
        "2026-01-02T10:00:00+00:00"
    );
    assert_eq!(
        conversation.ended_at.unwrap().to_rfc3339(),
        "2026-01-02T10:02:00+00:00"
    );

    // A new turn rewrites the header log and a chunk: the conversation is read again, whole.
    assert_eq!(reader.poll().unwrap().records, 0);
    put(
        &cursor.connection,
        "cursorDiskKV",
        "composerConvHeaders:c-chunked:3",
        &json!([{"bubbleId": "h3", "type": 2}, {"bubbleId": "h4", "type": 1}]),
    );
    cursor.bubble(
        "c-chunked",
        "h4",
        json!({"type": 1, "text": "Thanks", "createdAt": "2026-01-02T10:03:00.000Z"}),
    );
    let mut data: Value = serde_json::from_str(
        &cursor
            .connection
            .query_row(
                "SELECT value FROM cursorDiskKV WHERE key = 'composerData:c-chunked'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
    )
    .unwrap();
    data["headerLog"]["count"] = json!(4);
    data["lastUpdatedAt"] = json!(1_767_348_180_000_i64);
    cursor.composer("c-chunked", data);
    assert_eq!(reader.poll().unwrap().records, 1);
    assert_eq!(reader.conversation().messages.len(), 3);
    assert_eq!(reader.conversation().messages[2].text(), "Thanks");
}

#[test]
fn a_cursor_row_whose_key_is_not_text_does_not_stop_the_listing() {
    let dir = TempDir::new().unwrap();
    let cursor = CursorUser::create(&dir.path().join("User"));
    cursor.fill();
    // A key in the composer range that is not valid UTF-8, with a value that looks like a
    // conversation, and a bubble row of the same kind.
    let value =
        json!({"composerId": "x", "fullConversationHeadersOnly": [{"bubbleId": "b1", "type": 1}]});
    cursor
        .connection
        .execute(
            "INSERT INTO cursorDiskKV VALUES (CAST(X'636F6D706F736572446174613AFFFE' AS TEXT), ?1)",
            [value.to_string()],
        )
        .unwrap();
    cursor
        .connection
        .execute(
            "INSERT INTO cursorDiskKV VALUES (CAST(X'627562626C6549643A632D6167656E743AFFFE' AS TEXT), ?1)",
            [json!({"type": 1, "text": "odd"}).to_string()],
        )
        .unwrap();
    cursor
        .connection
        .execute(
            "INSERT INTO composerHeaders (composerId, isSubagent) VALUES (CAST(X'FFFE' AS TEXT), 1)",
            [],
        )
        .unwrap();
    let files = discover(Agent::Cursor, &cursor.root).unwrap();
    let ids: Vec<&str> = files.iter().map(|f| f.session_id.as_str()).collect();
    assert_eq!(ids, ["c-agent", "c-chat"]);
    let conversation = read_session(&files[0]).unwrap();
    assert_eq!(conversation.messages.len(), 2);
}

#[test]
fn a_cursor_composer_without_turn_times_starts_when_it_was_created() {
    let dir = TempDir::new().unwrap();
    let cursor = CursorUser::create(&dir.path().join("User"));
    cursor.composer(
        "c-old",
        json!({"_v": 2, "composerId": "c-old", "createdAt": 1_767_000_000_000_i64,
               "lastUpdatedAt": 1_767_000_060_000_i64,
               "conversation": [{"type": 1, "text": "Hello"}, {"type": 2, "text": "Hi"}]}),
    );
    let file = discover(Agent::Cursor, &cursor.root).unwrap().remove(0);
    let conversation = read_session(&file).unwrap();
    assert_eq!(
        conversation.started_at.unwrap().timestamp_millis(),
        1_767_000_000_000
    );
    assert_eq!(
        conversation.ended_at.unwrap().timestamp_millis(),
        1_767_000_060_000
    );
}

// Cline SDK sessions

/// Writes a Cline SDK session folder: its messages file and its manifest.
fn cline_sdk_session(root: &Path, id: &str, messages: Value) -> PathBuf {
    let dir = root.join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{id}.messages.json")),
        json!({"version": 1, "updated_at": "2026-01-02T10:01:00.000Z", "agent": "lead",
               "sessionId": id,
               "origin": {"source": "cli", "mode": "user", "sessionId": id},
               "messages": messages})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.join(format!("{id}.json")),
        json!({"version": 1, "session_id": id, "source": "cli", "pid": 1,
               "started_at": "2026-01-02T09:59:58.000Z", "status": "completed",
               "interactive": true, "provider": "example", "model": "model-example",
               "cwd": PROJECT, "workspace_root": PROJECT,
               "enable_tools": true, "enable_spawn": false, "enable_teams": false,
               "metadata": {"title": "Summarise the README", "mode": "act"},
               "messages_path": format!("/Users/example/.cline/data/sessions/{id}/{id}.messages.json")})
        .to_string(),
    )
    .unwrap();
    dir
}

fn cline_sdk_messages() -> Value {
    json!([
        {"id": "m1", "role": "user", "content": [
            {"type": "text", "text": "<mode_notice>The user switched from plan mode to act mode before sending this message.</mode_notice><user_input mode=\"act\">Summarise the README</user_input>"},
            {"type": "text", "text": "<file_content path=\"/Users/example/project/README.md\">\nfile contents that are never kept\n</file_content>"}
        ]},
        {"id": "m2", "role": "assistant", "ts": 1_767_348_000_000_i64,
         "modelInfo": {"id": "model-newer", "provider": "example"},
         "content": [
            {"type": "thinking", "thinking": "Private reasoning"},
            {"type": "text", "text": "I will read it first."},
            {"type": "tool_use", "id": "t1", "name": "read_files",
             "input": {"files": [{"path": "/Users/example/project/README.md", "start_line": 1}]}},
            {"type": "tool_use", "id": "t2", "name": "run_commands",
             "input": {"commands": ["cargo test", "cargo clippy"]}}
        ]},
        {"id": "m3", "role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": "output that is never kept", "is_error": false},
            {"type": "tool_result", "tool_use_id": "t2", "content": "error output", "is_error": true}
        ]},
        {"id": "m4", "role": "assistant", "ts": 1_767_348_010_000_i64,
         "modelInfo": {"id": "model-newer", "provider": "example"},
         "metrics": {"inputTokens": 1, "outputTokens": 1, "cacheReadTokens": 0, "cacheWriteTokens": 0, "cost": 0},
         "content": [{"type": "text", "text": "The README describes a small project."}]},
        {"id": "m5", "role": "user", "content": [
            {"type": "text", "text": "<user_command slash=\"newtask\">continue in a new task</user_command>"}
        ]}
    ])
}

#[test]
fn reads_a_cline_sdk_session() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("sessions");
    let id = "1767348000000_abcde";
    let session = cline_sdk_session(&root, id, cline_sdk_messages());
    // A subagent's messages in the same folder, and the session's other files, are not sessions.
    fs::write(
        session.join("agent-1.messages.json"),
        json!({"version": 1, "agent": "subagent", "sessionId": "sub", "messages": [
            {"role": "user", "content": [{"type": "text", "text": "a subagent task"}]}
        ]})
        .to_string(),
    )
    .unwrap();
    fs::write(session.join(format!("{id}.compaction.json")), "{}").unwrap();
    fs::write(session.join("hooks.jsonl"), "").unwrap();

    let files = discover(Agent::Cline, &root).unwrap();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(files[0].session_id, id);
    assert_eq!(files[0].path, session.join(format!("{id}.messages.json")));

    let conversation = read_session(&files[0]).unwrap();
    assert_eq!(conversation.agent, Agent::Cline);
    assert_eq!(conversation.session_id, id);
    assert_eq!(conversation.project.as_deref(), Some(Path::new(PROJECT)));
    assert_eq!(conversation.model.as_deref(), Some("model-newer"));
    assert_eq!(conversation.title(), "Summarise the README");
    let roles: Vec<Role> = conversation.messages.iter().map(|m| m.role).collect();
    assert_eq!(roles, [Role::User, Role::Assistant, Role::User]);
    assert_eq!(conversation.messages[0].text(), "Summarise the README");
    assert_eq!(
        tools(&conversation, 1),
        [
            tool("read_files", "/Users/example/project/README.md", false),
            tool("run_commands", "cargo test", true),
        ]
    );
    assert!(
        conversation.messages[1]
            .text()
            .ends_with("The README describes a small project.")
    );
    assert_eq!(
        conversation.messages[2].text(),
        "/newtask continue in a new task"
    );
    let markdown = to_markdown(&conversation);
    assert!(!markdown.contains("never kept") && !markdown.contains("Private reasoning"));
    assert!(!markdown.contains("mode_notice") && !markdown.contains("switched"));
    assert!(!markdown.contains("subagent task"));
    assert_eq!(
        conversation.started_at.unwrap().to_rfc3339(),
        "2026-01-02T09:59:58+00:00"
    );
    assert_eq!(
        conversation.ended_at.unwrap().to_rfc3339(),
        "2026-01-02T10:00:10+00:00"
    );
}

#[test]
fn reads_the_newer_cline_task_messages() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("tasks");
    task(
        &root,
        "1767348000000",
        json!([
            {"ts": 1_767_348_000_000_i64, "type": "say", "say": "task", "text": "Plan the refactor"},
            {"ts": 1_767_348_001_000_i64, "type": "ask", "ask": "plan_mode_respond",
             "text": "{\"response\":\"First split the module.\",\"options\":[]}"},
            {"ts": 1_767_348_002_000_i64, "type": "say", "say": "user_feedback", "text": "Go ahead"},
            {"ts": 1_767_348_003_000_i64, "type": "ask", "ask": "act_mode_respond",
             "text": "{\"response\":\"Splitting it now.\"}"},
            {"ts": 1_767_348_004_000_i64, "type": "say", "say": "plan_completion_result",
             "text": "The module is split."},
            {"ts": 1_767_348_005_000_i64, "type": "say", "say": "checkpoint_created"},
            {"ts": 1_767_348_006_000_i64, "type": "say", "say": "hook_status", "text": "{}"}
        ]),
    );
    let file = discover(Agent::Cline, &root).unwrap().remove(0);
    let conversation = read_session(&file).unwrap();
    let texts: Vec<String> = conversation.messages.iter().map(|m| m.text()).collect();
    assert_eq!(texts[0], "Plan the refactor");
    assert_eq!(texts[1], "First split the module.");
    assert_eq!(texts[2], "Go ahead");
    assert!(texts[3].starts_with("Splitting it now."));
    assert!(texts[3].ends_with("The module is split."));
    assert_eq!(texts.len(), 4);
}

/// The messages of an SDK session resumed from the classic task of [`cline_messages`]: a
/// converted copy of the classic turns (without times), then the turns written after resuming.
fn resumed_sdk_messages() -> Value {
    json!([
        {"id": "r1", "role": "user", "content": [{"type": "text", "text": "<task>Add a dark mode toggle</task>"}]},
        {"id": "r2", "role": "assistant", "content": [{"type": "text", "text": "I will look at the settings page first."}]},
        {"id": "r3", "role": "user", "content": [{"type": "text", "text": "Yes"}]},
        {"id": "r4", "role": "assistant", "content": [{"type": "text", "text": "The toggle follows the system setting."}]},
        {"id": "r5", "role": "user", "content": [{"type": "text", "text": "<user_input mode=\"act\">Now add a test</user_input>"}]},
        {"id": "r6", "role": "assistant", "ts": 1_767_348_300_000_i64,
         "modelInfo": {"id": "model-newer", "provider": "example"},
         "content": [{"type": "text", "text": "Added a test for the toggle."}]}
    ])
}

#[test]
fn a_classic_cline_task_resumed_on_the_sdk_is_one_conversation() {
    let dir = TempDir::new().unwrap();
    let data = dir.path().join(".cline/data");
    let id = "1767348000000";
    let tasks = data.join("tasks");
    let classic = task(&tasks, id, cline_messages());
    let sessions = data.join("sessions");
    let session = cline_sdk_session(&sessions, id, resumed_sdk_messages());

    let classic_file = discover(Agent::Cline, &tasks).unwrap().remove(0);
    let sdk_file = discover(Agent::Cline, &sessions).unwrap().remove(0);
    assert_eq!(classic_file.path, classic.join("ui_messages.json"));
    assert_eq!(sdk_file.path, session.join(format!("{id}.messages.json")));
    // Either file gives the same conversation.
    let from_classic = read_session(&classic_file).unwrap();
    let from_sdk = read_session(&sdk_file).unwrap();
    assert_eq!(from_classic, from_sdk);

    let texts: Vec<String> = from_sdk.messages.iter().map(|m| m.text()).collect();
    assert_eq!(texts.len(), 6, "{texts:?}");
    assert_eq!(texts[0], "Add a dark mode toggle");
    assert_eq!(texts[3], "The toggle follows the system setting.");
    assert_eq!(texts[4], "Now add a test");
    assert_eq!(texts[5], "Added a test for the toggle.");
    // The converted copy of the classic turns is not added again.
    let markdown = to_markdown(&from_sdk);
    assert_eq!(
        markdown
            .matches("I will look at the settings page first.")
            .count(),
        1
    );
    // The classic tool calls are kept; the SDK's title and model win.
    assert_eq!(tools(&from_sdk, 1).len(), 4);
    assert_eq!(from_sdk.title(), "Summarise the README");
    assert_eq!(from_sdk.model.as_deref(), Some("model-newer"));
    assert_eq!(from_sdk.project.as_deref(), Some(Path::new(PROJECT)));
    assert_eq!(
        from_sdk.started_at.unwrap().to_rfc3339(),
        "2026-01-02T09:59:58+00:00"
    );
    assert_eq!(
        from_sdk.ended_at.unwrap().to_rfc3339(),
        "2026-01-02T10:05:00+00:00"
    );

    // An SDK session with nothing after the classic task adds no turns.
    cline_sdk_session(
        &sessions,
        id,
        json!([{"id": "q1", "role": "user", "content": [{"type": "text", "text": "Add a dark mode toggle"}]}]),
    );
    assert_eq!(read_session(&classic_file).unwrap().messages.len(), 4);
}

#[test]
fn captures_cline_sdk_sessions_once_and_follows_them() {
    let id = "1767348000000";
    let capture_in_order = |sessions_first: bool| {
        let dir = TempDir::new().unwrap();
        let mut space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
        let data = dir.path().join(".cline/data");
        let sessions = data.join("sessions");
        let session = cline_sdk_session(&sessions, id, resumed_sdk_messages());
        // The same task, from before Cline moved to its SDK: a resumed task keeps its id.
        let tasks = data.join("tasks");
        task(&tasks, id, cline_messages());
        let mut roots = vec![(Agent::Cline, tasks), (Agent::Cline, sessions)];
        if sessions_first {
            roots.reverse();
        }
        let mut capture = Capture::open(dir.path().join("state.json"))
            .unwrap()
            .with_roots(roots);
        let report = capture.sync_all(&mut space).unwrap();
        assert!(report.failed.is_empty(), "{report:?}");
        // One conversation, one node.
        let nodes: std::collections::BTreeSet<_> =
            report.changes.iter().map(|c| c.node_id).collect();
        assert_eq!(nodes.len(), 1, "{report:?}");
        let node_id = nodes.into_iter().next().unwrap();
        for (key, value) in [
            (property_keys::CONVERSATION_AGENT, "cline"),
            (property_keys::CONVERSATION_SESSION, id),
            (property_keys::CONVERSATION_PROJECT, PROJECT),
        ] {
            assert_eq!(
                space.property(&node_id, key).unwrap().as_deref(),
                Some(value),
                "{key}"
            );
        }
        assert!(
            space
                .property(&node_id, property_keys::CONVERSATION_STARTED_AT)
                .unwrap()
                .is_some()
        );
        let node = space.get_node(&node_id).unwrap().unwrap();
        assert_eq!(node.title, "Summarise the README");
        assert!(node.body.contains("Add a dark mode toggle"));
        assert!(node.body.contains("Added a test for the toggle."));

        // Nothing new: nothing written.
        let again = capture.sync_all(&mut space).unwrap();
        assert!(again.changes.is_empty(), "{again:?}");

        // The session grows: the same node is updated, and keeps the classic turns.
        let mut messages = resumed_sdk_messages();
        messages.as_array_mut().unwrap().push(json!(
            {"id": "r7", "role": "assistant", "ts": 1_767_348_400_000_i64,
             "content": [{"type": "text", "text": "The test passes."}]}
        ));
        let file = session.join(format!("{id}.messages.json"));
        fs::write(
            &file,
            json!({"version": 1, "agent": "lead", "sessionId": id, "messages": messages})
                .to_string(),
        )
        .unwrap();
        let report = capture.sync_paths(&mut space, &[file]).unwrap();
        assert_eq!(report.updated().count(), 1, "{report:?}");
        assert_eq!(report.created().count(), 0, "{report:?}");
        assert_eq!(report.updated().next().unwrap().node_id, node_id);
        let body = space.get_node(&node_id).unwrap().unwrap().body;
        assert!(body.contains("I will look at the settings page first."));
        assert!(body.contains("The test passes."));
        body
    };
    // The node is the same whichever layout is read first.
    assert_eq!(capture_in_order(false), capture_in_order(true));
}
