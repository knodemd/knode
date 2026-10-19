//! Windsurf chats, in the SQLite database `state.vscdb` of each folder in Windsurf's
//! `User/workspaceStorage` folder.
//!
//! Unverified format: no Windsurf data was available to check this reader against, so it
//! follows public descriptions and reads tolerantly. Each workspace database is read as
//! described in [`crate::vscdb`]; its `ItemTable` holds the chats of that window under
//! `workbench.panel.aichat.view.aichat.chatdata` (or `aiChat.chatdata`), a JSON object with
//! `tabs[]`: `tabId`, `chatTitle`, `lastSendTime` (milliseconds) and `bubbles[]`, each with a
//! `type` (`user` or `ai`, or `1` and `2`) and its text in `text`, `rawText` or `initText`. The
//! folder's `workspace.json` names the project.
//!
//! Cascade, Windsurf's agent, keeps its conversations in `~/.codeium/windsurf/cascade` as
//! binary `.pb` files whose format is not public; they are not read.
//!
//! A chat is read again whole when its tab changes; its version is a hash of the tab.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use rusqlite::Connection;
use serde_json::Value;

use crate::discover::SessionFile;
use crate::model::{Agent, Conversation, Part, Role, Title, TitleSource};
use crate::text::{any_time, first_str, one_line, str_field};
use crate::vscdb::{self, DATABASE};

/// The `ItemTable` keys that hold the chats of a window.
const CHAT_KEYS: [&str; 2] = [
    "workbench.panel.aichat.view.aichat.chatdata",
    "aiChat.chatdata",
];

/// The chats in every workspace database under `root` (Windsurf's `User/workspaceStorage`).
/// A database that cannot be read is left out; a missing root gives none.
pub(crate) fn list(root: &Path) -> io::Result<Vec<SessionFile>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if dir.join(DATABASE).is_file() {
            if let Ok(found) = list_workspace(&dir) {
                files.extend(found);
            }
        }
    }
    Ok(files)
}

/// The chats in the database of one workspace folder.
pub(crate) fn list_workspace(dir: &Path) -> io::Result<Vec<SessionFile>> {
    let connection = match vscdb::open(&dir.join(DATABASE)) {
        Ok(connection) => connection,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut files = Vec::new();
    for tab in tabs(&connection)? {
        let Some(id) = first_str(&tab, &["tabId"]) else {
            continue;
        };
        if !tab
            .get("bubbles")
            .and_then(Value::as_array)
            .is_some_and(|bubbles| !bubbles.is_empty())
        {
            continue;
        }
        files.push(SessionFile {
            agent: Agent::Windsurf,
            path: vscdb::session_path(dir, id),
            session_id: id.to_string(),
            len: tab_version(&tab),
            modified: tab
                .get("lastSendTime")
                .and_then(Value::as_u64)
                .map(|ms| UNIX_EPOCH + Duration::from_millis(ms)),
        });
    }
    Ok(files)
}

/// Every chat tab stored in the database.
fn tabs(connection: &Connection) -> io::Result<Vec<Value>> {
    let mut tabs = Vec::new();
    for key in CHAT_KEYS {
        let Some(bytes) = vscdb::item(connection, key)? else {
            continue;
        };
        let Ok(data) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if let Some(Value::Array(found)) = data.get("tabs") {
            tabs.extend(found.iter().cloned());
        }
    }
    Ok(tabs)
}

fn tab_version(tab: &Value) -> u64 {
    vscdb::hash(tab.to_string().as_bytes())
}

fn find_tab(path: &Path) -> io::Result<Option<(PathBuf, Value)>> {
    let (database, id) = vscdb::split(path).ok_or_else(|| not_a_session(path))?;
    let connection = vscdb::open(&database)?;
    let tab = tabs(&connection)?
        .into_iter()
        .find(|tab| str_field(tab, "tabId") == Some(id.as_str()));
    Ok(tab.map(|tab| (database, tab)))
}

fn not_a_session(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("{} is not a Windsurf chat", path.display()),
    )
}

/// The version of a chat, or `None` if it is gone.
pub(crate) fn version(path: &Path) -> io::Result<Option<u64>> {
    Ok(find_tab(path)?.map(|(_, tab)| tab_version(&tab)))
}

/// Reads the whole chat at `path` into `conversation`, which should be empty, and returns its
/// version.
pub(crate) fn read(path: &Path, conversation: &mut Conversation) -> io::Result<u64> {
    let (database, tab) = find_tab(path)?.ok_or_else(|| not_a_session(path))?;
    if let Some(id) = first_str(&tab, &["tabId"]) {
        conversation.session_id = id.to_string();
    }
    conversation.project = database.parent().and_then(vscdb::workspace_folder);
    if let Some(title) = first_str(&tab, &["chatTitle"]) {
        conversation.recorded_title = Some(Title {
            text: one_line(title, 200),
            source: TitleSource::Agent,
        });
    }
    let sent = any_time(tab.get("lastSendTime"));
    if let Some(at) = sent {
        conversation.saw_time(at);
    }
    for bubble in tab
        .get("bubbles")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let role = match bubble.get("type") {
            Some(Value::String(kind)) => match kind.to_ascii_lowercase().as_str() {
                "user" | "1" => Role::User,
                "ai" | "assistant" | "2" => Role::Assistant,
                _ => continue,
            },
            Some(Value::Number(kind)) => match kind.as_i64() {
                Some(1) => Role::User,
                Some(2) => Role::Assistant,
                _ => continue,
            },
            _ => continue,
        };
        let at = any_time(bubble.get("timestamp")).or(any_time(bubble.get("createdAt")));
        if let Some(at) = at {
            conversation.saw_time(at);
        }
        if let Some(text) = first_str(bubble, &["text", "rawText", "initText"]) {
            let text = text.trim_end().to_string();
            conversation.push_part(role, at, Part::Text { text });
        }
    }
    Ok(tab_version(&tab))
}
