//! Cursor CLI (`cursor-agent`) chats, one SQLite database per chat in
//! `~/.cursor/chats/<workspace hash>/<chat id>/store.db`, apart from the editor's
//! `state.vscdb` (read by [`crate::cursor`]), so the two never hold the same conversation.
//!
//! The database is read as described in [`crate::vscdb`]: read-only, without locks that get in
//! the CLI's way, and never copied. It has two tables:
//!
//! - `meta (key TEXT PRIMARY KEY, value TEXT)`: one row, key `0`, whose value is JSON written
//!   as hexadecimal text: `agentId`, `name` (the title), `createdAt` (milliseconds), `mode`,
//!   `lastUsedModel`, `latestRootBlobId` and `blobEncryptionKey`.
//! - `blobs (id TEXT PRIMARY KEY, data BLOB)`: a content-addressed tree whose root is
//!   `latestRootBlobId`; ids are the hexadecimal of a 32-byte digest. Inner nodes are protobuf:
//!   field 13 leads to the older part of the chat (a chain of nodes linked by field 4) and
//!   field 1 lists, in order, the 32-byte ids of the newer messages. Leaves are JSON messages
//!   in the Vercel AI SDK shape: `role` (`system`, `user`, `assistant` or `tool`) and `content`,
//!   a string or blocks of `type` `text`, `reasoning`, `tool-call` (`toolCallId`, `toolName`,
//!   `args`) or `tool-result`. The order of the rows is not the order of the chat.
//!
//! This layout is the one open-source readers of `cursor-agent` describe (2026 versions); it
//! was not checked against an install, and is read tolerantly: a blob that is neither a message
//! nor a node is left out. The person's words are the `<user_query>` of a user message when it
//! has one; context the CLI adds (`<user_info>` and other tags) is left out. The project is the
//! `file://` workspace in the root node, else the `Workspace Path:` the CLI tells the model,
//! else `cwd` in a `meta.json` next to the database. Thinking and tool results are left out.
//! Messages carry no time: the chat starts at `createdAt` and ends at the last change of its
//! files.
//!
//! A chat is read again whole when its root changes, which happens with every turn; its version
//! is a hash of the root id.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use crate::discover::SessionFile;
use crate::model::{Agent, Conversation, Part, Role, Title, TitleSource, ToolCall};
use crate::text::{SUMMARY_MAX, any_time, first_str, one_line, tag_content};
use crate::vscdb;

/// The file name of a chat's database.
pub(crate) const DATABASE: &str = "store.db";

/// How deep the tree of blobs is followed: far more than a chat needs, and a stop for a tree
/// that loops.
const MAX_DEPTH: usize = 64;

/// Whether `path` is a chat database or its write-ahead log.
pub(crate) fn is_database_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix(DATABASE))
        .is_some_and(|rest| matches!(rest, "" | "-wal"))
}

/// The database of the chat whose database or log is at `path`.
pub(crate) fn database_of(path: &Path) -> Option<PathBuf> {
    is_database_file(path).then(|| path.with_file_name(DATABASE))
}

/// The chat in the database at `path`, if it has at least one message.
pub(crate) fn session(path: &Path) -> Option<SessionFile> {
    let connection = vscdb::open(path).ok()?;
    let meta = meta(&connection).ok()??;
    let root = root_id(&meta)?;
    if !has_message(&connection, root) {
        return None;
    }
    Some(SessionFile {
        agent: Agent::CursorCli,
        path: path.to_path_buf(),
        session_id: session_id(path, &meta),
        len: vscdb::hash(root.as_bytes()),
        modified: modified(path),
    })
}

/// The chat's id: the name of its folder, else its `agentId`.
fn session_id(path: &Path, meta: &Value) -> String {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .or_else(|| first_str(meta, &["agentId"]).map(str::to_string))
        .unwrap_or_default()
}

/// The last change of the database or its log.
fn modified(path: &Path) -> Option<SystemTime> {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    [path.to_path_buf(), PathBuf::from(wal)]
        .iter()
        .filter_map(|file| fs::metadata(file).ok()?.modified().ok())
        .max()
}

/// The `meta` row, decoded from hexadecimal (or read as JSON when it is not).
fn meta(connection: &Connection) -> io::Result<Option<Value>> {
    if !vscdb::has_table(connection, "meta")? || !vscdb::has_table(connection, "blobs")? {
        return Ok(None);
    }
    let value: Option<String> = connection
        .query_row("SELECT value FROM meta WHERE key = '0'", [], |row| {
            row.get::<_, Option<String>>(0)
        })
        .optional()
        .map_err(io::Error::other)?
        .flatten();
    let Some(value) = value else {
        return Ok(None);
    };
    let text = value.trim();
    let bytes = hex(text).unwrap_or_else(|| text.as_bytes().to_vec());
    Ok(serde_json::from_slice(&bytes).ok())
}

fn root_id(meta: &Value) -> Option<&str> {
    first_str(meta, &["latestRootBlobId"]).filter(|id| !id.is_empty())
}

/// Bytes from hexadecimal text, or `None` when it is not.
fn hex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 || text.is_empty() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn blob(connection: &Connection, id: &str) -> Option<Vec<u8>> {
    connection
        .query_row("SELECT data FROM blobs WHERE id = ?1", [id], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .optional()
        .ok()
        .flatten()
}

/// The fields of a protobuf message, as (field number, wire type, bytes of a length-delimited
/// value). `None` when the bytes are not protobuf.
fn fields(mut bytes: &[u8]) -> Option<Vec<(u64, Vec<u8>)>> {
    fn varint(bytes: &mut &[u8]) -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let (&byte, rest) = bytes.split_first()?;
            *bytes = rest;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let key = varint(&mut bytes)?;
        let (field, wire) = (key >> 3, key & 7);
        if field == 0 {
            return None;
        }
        match wire {
            0 => {
                varint(&mut bytes)?;
            }
            1 => bytes = bytes.get(8..)?,
            2 => {
                let len = usize::try_from(varint(&mut bytes)?).ok()?;
                out.push((field, bytes.get(..len)?.to_vec()));
                bytes = &bytes[len..];
            }
            5 => bytes = bytes.get(4..)?,
            _ => return None,
        }
    }
    Some(out)
}

/// What a blob is.
enum Node {
    Message(Value),
    Inner(Vec<(u64, Vec<u8>)>),
}

fn node(bytes: &[u8]) -> Option<Node> {
    if bytes.first().is_some_and(|b| *b == b'{') {
        if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
            return value.get("role").is_some().then_some(Node::Message(value));
        }
    }
    fields(bytes).map(Node::Inner)
}

/// The messages of the chat, in order, and the `file://` workspaces the nodes name.
struct Walk<'a> {
    connection: &'a Connection,
    seen: HashSet<String>,
    messages: Vec<Value>,
    workspace: Option<PathBuf>,
}

impl Walk<'_> {
    fn visit(&mut self, id: &str, depth: usize) {
        if depth > MAX_DEPTH || !self.seen.insert(id.to_string()) {
            return;
        }
        let Some(bytes) = blob(self.connection, id) else {
            return;
        };
        match node(&bytes) {
            Some(Node::Message(message)) => self.messages.push(message),
            Some(Node::Inner(fields)) => self.inner(&fields, depth),
            None => {}
        }
    }

    /// An inner node: the older part first (field 13, then the chain of field 4), then the
    /// messages it lists (field 1).
    fn inner(&mut self, fields: &[(u64, Vec<u8>)], depth: usize) {
        for (number, value) in fields {
            if self.workspace.is_none() {
                if let Some(path) = std::str::from_utf8(value)
                    .ok()
                    .filter(|text| text.starts_with("file://"))
                    .and_then(vscdb::file_uri_path)
                {
                    self.workspace = Some(path);
                }
            }
            if matches!(number, 13 | 4) {
                self.reference(value, depth);
            }
        }
        for (number, value) in fields {
            if *number == 1 {
                self.reference(value, depth);
            }
        }
    }

    /// A 32-byte id, or a nested node holding ids.
    fn reference(&mut self, value: &[u8], depth: usize) {
        if value.len() == 32 {
            self.visit(&to_hex(value), depth + 1);
        } else if let Some(fields) = fields(value) {
            self.inner(&fields, depth + 1);
        }
    }
}

fn walk<'a>(connection: &'a Connection, root: &str) -> Walk<'a> {
    let mut walk = Walk {
        connection,
        seen: HashSet::new(),
        messages: Vec::new(),
        workspace: None,
    };
    walk.visit(root, 0);
    walk
}

fn has_message(connection: &Connection, root: &str) -> bool {
    walk(connection, root).messages.iter().any(|m| {
        matches!(
            m.get("role").and_then(Value::as_str),
            Some("user" | "assistant")
        )
    })
}

fn not_a_session(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("{} is not a Cursor CLI chat", path.display()),
    )
}

/// The version of the chat at `path`, or `None` if it is gone.
pub(crate) fn version(path: &Path) -> io::Result<Option<u64>> {
    let connection = match vscdb::open(path) {
        Ok(connection) => connection,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(meta(&connection)?
        .as_ref()
        .and_then(root_id)
        .map(|root| vscdb::hash(root.as_bytes())))
}

/// Reads the whole chat at `path` into `conversation`, which should be empty, and returns its
/// version.
pub(crate) fn read(path: &Path, conversation: &mut Conversation) -> io::Result<u64> {
    let connection = vscdb::open(path)?;
    let meta = meta(&connection)?.ok_or_else(|| not_a_session(path))?;
    let root = root_id(&meta).ok_or_else(|| not_a_session(path))?;
    conversation.session_id = session_id(path, &meta);
    if let Some(name) = first_str(&meta, &["name"]).filter(|name| !name.trim().is_empty()) {
        conversation.recorded_title = Some(Title {
            text: one_line(name, 200),
            source: TitleSource::Agent,
        });
    }
    if let Some(model) = first_str(&meta, &["lastUsedModel"])
        .filter(|model| !model.is_empty() && *model != "default")
    {
        conversation.model = Some(model.to_string());
    }
    let started = any_time(meta.get("createdAt"));
    let walk = walk(&connection, root);
    let mut workspace = walk.workspace;
    for message in &walk.messages {
        apply(conversation, message, started, &mut workspace);
    }
    if conversation.messages.is_empty() {
        return Err(not_a_session(path));
    }
    if let Some(at) = started {
        conversation.saw_time(at);
    }
    if let Some(at) = modified(path).map(DateTime::<Utc>::from) {
        conversation.saw_time(at);
    }
    conversation.project = workspace.or_else(|| meta_json_cwd(path));
    Ok(vscdb::hash(root.as_bytes()))
}

/// The text blocks of a message's content.
fn texts(content: Option<&Value>) -> Vec<&str> {
    match content {
        Some(Value::String(text)) => vec![text.as_str()],
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect(),
        _ => Vec::new(),
    }
}

fn apply(
    conversation: &mut Conversation,
    message: &Value,
    at: Option<DateTime<Utc>>,
    workspace: &mut Option<PathBuf>,
) {
    let content = message.get("content");
    match message.get("role").and_then(Value::as_str) {
        Some("user") => {
            for text in texts(content) {
                if workspace.is_none() {
                    *workspace = workspace_path(text);
                }
                let typed = match tag_content(text, "user_query") {
                    Some(query) => query,
                    None if crate::text::starts_with_tag(text) => continue,
                    None => text.trim(),
                };
                if !typed.is_empty() {
                    let text = typed.to_string();
                    conversation.push_part(Role::User, at, Part::Text { text });
                }
            }
        }
        Some("assistant") => {
            let blocks: Vec<Value> = match content {
                Some(Value::Array(blocks)) => blocks.clone(),
                Some(text @ Value::String(_)) => {
                    vec![serde_json::json!({"type": "text", "text": text})]
                }
                _ => Vec::new(),
            };
            for block in &blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = block
                            .get("text")
                            .and_then(Value::as_str)
                            .map(str::trim_end)
                            .filter(|text| !text.trim().is_empty())
                        {
                            let text = text.to_string();
                            conversation.push_part(Role::Assistant, at, Part::Text { text });
                        }
                    }
                    Some("tool-call") => {
                        let name = first_str(block, &["toolName"]).unwrap_or("tool");
                        conversation.push_part(
                            Role::Assistant,
                            at,
                            Part::Tool(ToolCall {
                                id: first_str(block, &["toolCallId"]).map(str::to_string),
                                name: name.to_string(),
                                summary: summarise(block.get("args").unwrap_or(&Value::Null)),
                                failed: false,
                            }),
                        );
                    }
                    _ => {}
                }
            }
        }
        // The CLI's instructions may name the workspace; they and tool results are what the
        // model saw, not the chat.
        Some("system") if workspace.is_none() => {
            *workspace = texts(content).into_iter().find_map(workspace_path);
        }
        _ => {}
    }
}

/// The `Workspace Path: /...` line the CLI tells the model.
fn workspace_path(text: &str) -> Option<PathBuf> {
    text.lines().find_map(|line| {
        let path = line.trim().strip_prefix("Workspace Path:")?.trim();
        Path::new(path).is_absolute().then(|| PathBuf::from(path))
    })
}

/// `cwd` in a `meta.json` next to the database.
fn meta_json_cwd(path: &Path) -> Option<PathBuf> {
    let bytes = fs::read(path.with_file_name("meta.json")).ok()?;
    let meta: Value = serde_json::from_slice(&bytes).ok()?;
    let cwd = first_str(&meta, &["cwd", "workspacePath"])?;
    Path::new(cwd).is_absolute().then(|| PathBuf::from(cwd))
}

fn summarise(args: &Value) -> String {
    let summary = first_str(
        args,
        &[
            "command",
            "path",
            "file_path",
            "filePath",
            "target_file",
            "pattern",
            "query",
            "url",
            "description",
        ],
    )
    .unwrap_or_default();
    one_line(summary, SUMMARY_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_hex_and_protobuf() {
        assert_eq!(hex("7b7d"), Some(b"{}".to_vec()));
        assert_eq!(hex("7b7"), None);
        assert_eq!(hex("zz"), None);
        // Field 1, length 2, "hi"; field 2, varint 5.
        let parsed = fields(&[0x0a, 2, b'h', b'i', 0x10, 5]).unwrap();
        assert_eq!(parsed, vec![(1, b"hi".to_vec())]);
        assert!(fields(&[0x0a, 9, b'h']).is_none());
    }

    #[test]
    fn finds_the_workspace_path() {
        assert_eq!(
            workspace_path("<user_info>\nOS: darwin\nWorkspace Path: /work/garden\n</user_info>"),
            Some(PathBuf::from("/work/garden"))
        );
        assert_eq!(workspace_path("Workspace Path: relative"), None);
    }
}
