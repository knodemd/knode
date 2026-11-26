//! The Cursor CLI (`cursor-agent`): discovery, reading and capture, against synthetic
//! `store.db` files built here in the shape public readers describe.

use std::fs;
use std::path::{Path, PathBuf};

use knode_format::{Space, kinds, property_keys};
use knode_ingest::{Agent, Capture, Role, SessionReader, discover};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

/// A chat database, as `cursor-agent` keeps it.
struct Store {
    path: PathBuf,
    connection: Connection,
}

fn length_delimited(field: u8, bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![(field << 3) | 2];
    let mut len = bytes.len();
    loop {
        let byte = (len & 0x7f) as u8;
        len >>= 7;
        if len == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    out.extend_from_slice(bytes);
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Store {
    fn create(root: &Path, workspace: &str, chat: &str) -> Store {
        let dir = root.join(workspace).join(chat);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);
                 CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);",
            )
            .unwrap();
        Store { path, connection }
    }

    /// Stores a blob and returns its 32-byte id.
    fn blob(&self, data: &[u8]) -> [u8; 32] {
        let id: [u8; 32] = Sha256::digest(data).into();
        self.connection
            .execute(
                "INSERT OR REPLACE INTO blobs (id, data) VALUES (?1, ?2)",
                params![hex(&id), data],
            )
            .unwrap();
        id
    }

    fn message(&self, message: Value) -> [u8; 32] {
        self.blob(message.to_string().as_bytes())
    }

    /// An inner node listing `children` in field 1, with `older` in field 13 and `extra`.
    fn node(&self, older: Option<[u8; 32]>, children: &[[u8; 32]], extra: &[u8]) -> [u8; 32] {
        let mut data = Vec::new();
        if let Some(older) = older {
            data.extend(length_delimited(13, &older));
        }
        for child in children {
            data.extend(length_delimited(1, child));
        }
        data.extend_from_slice(extra);
        self.blob(&data)
    }

    fn meta(&self, root: [u8; 32], name: &str) {
        let meta = json!({
            "agentId": "agent-1",
            "name": name,
            "createdAt": 1_767_348_000_000_i64,
            "mode": "default",
            "lastUsedModel": "gpt-5",
            "latestRootBlobId": hex(&root),
            "blobEncryptionKey": "not used",
        });
        self.connection
            .execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('0', ?1)",
                [hex(meta.to_string().as_bytes())],
            )
            .unwrap();
    }

    /// A chat of two turns: the first in an older node, the second in the root.
    /// Returns the older node and the messages of the root.
    fn fill(&self) -> ([u8; 32], Vec<[u8; 32]>) {
        let system = self.message(json!({"role": "system", "content": "You are an agent."}));
        let first = self.message(json!({"role": "user", "content": [{"type": "text", "text":
            "<user_info>\nOS Version: darwin\nWorkspace Path: /Users/example/other\n</user_info>\n\n<user_query>\nWhy does the build fail?\n</user_query>"}]}));
        let answer = self.message(json!({"role": "assistant", "content": [
            {"type": "reasoning", "text": "Thinking about it."},
            {"type": "text", "text": "Let me look."},
            {"type": "tool-call", "toolCallId": "call-1", "toolName": "Shell",
             "args": {"command": "cargo build"}}
        ]}));
        let result = self.message(json!({"role": "tool", "content": [
            {"type": "tool-result", "toolCallId": "call-1", "result": "error"}]}));
        let second = self.message(json!({"role": "user", "content": "Fix it, please."}));
        let done = self.message(json!({"role": "assistant", "content": "Fixed the import."}));
        // Something that is neither a message nor a node.
        let opaque = self.blob(&[0xff, 0xfe, 0x00]);
        let older = self.node(None, &[system, first, answer, result], &[]);
        let root = self.node(
            Some(older),
            &[second, done, opaque],
            &length_delimited(5, b"file:///Users/example/project"),
        );
        self.meta(root, "Fix the build");
        (older, vec![second, done])
    }
}

#[test]
fn reads_a_cursor_cli_chat_in_order() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("chats");
    let store = Store::create(&root, "5f2b", "chat-1");
    let (older, newer) = store.fill();
    // A chat with no message yet is left out.
    let empty = Store::create(&root, "5f2b", "chat-empty");
    let node = empty.node(None, &[], &[]);
    empty.meta(node, "New chat");
    fs::create_dir_all(root.join("5f2b/no-database")).unwrap();

    let files = discover(Agent::CursorCli, &root).unwrap();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(files[0].path, store.path);
    assert_eq!(files[0].session_id, "chat-1");

    let mut reader = SessionReader::new(&files[0]);
    reader.poll().unwrap();
    let conversation = reader.conversation();
    assert_eq!(conversation.agent, Agent::CursorCli);
    assert_eq!(conversation.title(), "Fix the build");
    assert_eq!(
        conversation.project.as_deref(),
        Some(Path::new("/Users/example/project"))
    );
    assert_eq!(conversation.model.as_deref(), Some("gpt-5"));
    assert_eq!(
        conversation.started_at.map(|t| t.timestamp_millis()),
        Some(1_767_348_000_000)
    );
    let turns: Vec<(Role, String)> = conversation
        .messages
        .iter()
        .map(|m| (m.role, m.text()))
        .collect();
    assert_eq!(turns.len(), 4, "{turns:?}");
    assert_eq!(turns[0], (Role::User, "Why does the build fail?".into()));
    assert_eq!(turns[1].0, Role::Assistant);
    assert!(turns[1].1.contains("Let me look."), "{turns:?}");
    assert!(!turns[1].1.contains("Thinking"), "{turns:?}");
    let tools: Vec<_> = conversation.messages[1].tool_calls().collect();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "Shell");
    assert_eq!(tools[0].summary, "cargo build");
    assert_eq!(turns[2], (Role::User, "Fix it, please.".into()));
    assert_eq!(turns[3], (Role::Assistant, "Fixed the import.".into()));
    assert_eq!(reader.poll().unwrap().records, 0, "nothing changed");

    // A new turn makes a new root: the chat is read again.
    let ask = store.message(json!({"role": "user", "content": "Thanks"}));
    let mut children = newer;
    children.push(ask);
    let root_id = store.node(Some(older), &children, &[]);
    store.meta(root_id, "Fix the build");
    reader.poll().unwrap();
    assert_eq!(reader.conversation().messages.len(), 5);
    assert_eq!(
        reader.conversation().messages.last().unwrap().text(),
        "Thanks"
    );
}

#[test]
fn captures_cursor_cli_chats_into_a_space_and_follows_them() {
    let dir = TempDir::new().unwrap();
    let mut space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
    let root = dir.path().join("chats");
    let store = Store::create(&root, "5f2b", "chat-1");
    let (older, newer) = store.fill();

    let mut capture = Capture::open(dir.path().join("state.json"))
        .unwrap()
        .with_roots([(Agent::CursorCli, root.clone())]);
    let report = capture.sync_all(&mut space).unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    let created: Vec<_> = report.created().collect();
    assert_eq!(created.len(), 1, "{report:?}");
    let id = created[0].node_id;
    assert_eq!(
        space.get_node(&id).unwrap().unwrap().kind,
        kinds::CONVERSATION
    );
    assert_eq!(
        space
            .property(&id, property_keys::CONVERSATION_AGENT)
            .unwrap()
            .as_deref(),
        Some("cursor-cli")
    );
    assert!(capture.sync_all(&mut space).unwrap().changes.is_empty());

    // A turn written to the log is found from it.
    let mut children = newer;
    children.push(store.message(json!({"role": "user", "content": "And the tests?"})));
    let root_id = store.node(Some(older), &children, &[]);
    store.meta(root_id, "Fix the build and the tests");
    let wal = store.path.with_file_name("store.db-wal");
    let report = capture.sync_paths(&mut space, &[wal]).unwrap();
    assert_eq!(report.updated().count(), 1, "{report:?}");
    assert_eq!(
        space.get_node(&id).unwrap().unwrap().title,
        "Fix the build and the tests"
    );
}
