//! OpenCode sessions, in the SQLite database `~/.local/share/opencode/opencode.db`.
//!
//! The database is only ever opened read-only. The tables read here:
//!
//! - `session`: `id`, `parent_id` (set for the sessions of subagents, which are left out),
//!   `directory`, `title`, `time_created` and `time_updated` (milliseconds);
//! - `message`: `id`, `session_id`, `time_created` and `data`, a JSON object with `role`,
//!   `modelID`, `path.cwd`, and `mode` or `summary` for compaction summaries;
//! - `part`: `id`, `message_id`, `session_id` and `data`, a JSON object with a `type`: `text`
//!   (`text`, `synthetic` for context OpenCode adds), `tool` (`callID`, `tool`, `state.input`,
//!   `state.status`), `file` (`mime`), and others (reasoning, steps, patches) that are ignored.
//!
//! Parts change in place while a session runs (a tool call finishes, text streams in), so a
//! session is read again whole whenever its `time_updated` moves; the session list is cheap to
//! check. Each session is known by a path of its own, `<folder>/opencode.db#<session id>`, so
//! capture can follow every session in the one database.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;

use crate::discover::SessionFile;
use crate::model::{Agent, Conversation, Part, Role, Title, TitleSource, ToolCall};
use crate::text::{SUMMARY_MAX, first_str, one_line, str_field};

/// The database file name.
pub(crate) const DATABASE: &str = "opencode.db";

/// The path that stands for one session.
pub(crate) fn session_path(root: &Path, id: &str) -> PathBuf {
    root.join(format!("{DATABASE}#{id}"))
}

/// The database and session id a session path stands for.
pub(crate) fn split(path: &Path) -> Option<(PathBuf, String)> {
    let name = path.file_name()?.to_str()?;
    let id = name.strip_prefix(DATABASE)?.strip_prefix('#')?;
    Some((path.with_file_name(DATABASE), id.to_string()))
}

/// Whether a changed file is the database or one of its journal files.
pub(crate) fn is_database_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(DATABASE) && !name.contains('#'))
}

fn open(database: &Path) -> io::Result<Connection> {
    if !database.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} does not exist", database.display()),
        ));
    }
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(io::Error::other)?;
    connection
        .busy_timeout(Duration::from_secs(2))
        .map_err(io::Error::other)?;
    Ok(connection)
}

/// The top-level sessions in the database under `root`. A missing database gives none.
pub(crate) fn list(root: &Path) -> io::Result<Vec<SessionFile>> {
    let connection = match open(&root.join(DATABASE)) {
        Ok(connection) => connection,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut statement = connection
        .prepare("SELECT id, time_updated FROM session WHERE parent_id IS NULL")
        .map_err(io::Error::other)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(io::Error::other)?;
    let mut files = Vec::new();
    for row in rows {
        let (id, updated) = row.map_err(io::Error::other)?;
        let updated = u64::try_from(updated).unwrap_or(0);
        files.push(SessionFile {
            agent: Agent::OpenCode,
            path: session_path(root, &id),
            session_id: id,
            len: updated,
            modified: Some(UNIX_EPOCH + Duration::from_millis(updated)),
        });
    }
    Ok(files)
}

/// The version of a session: its `time_updated`, or `None` if it is gone.
pub(crate) fn version(path: &Path) -> io::Result<Option<u64>> {
    let (database, id) = split(path).ok_or_else(|| not_a_session(path))?;
    let connection = open(&database)?;
    let updated: Option<i64> = connection
        .query_row(
            "SELECT time_updated FROM session WHERE id = ?1",
            [&id],
            |row| row.get(0),
        )
        .optional()
        .map_err(io::Error::other)?;
    Ok(updated.map(|updated| u64::try_from(updated).unwrap_or(0)))
}

fn not_a_session(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("{} is not an OpenCode session", path.display()),
    )
}

/// Reads the whole session at `path` into `conversation`, which should be empty.
pub(crate) fn read(path: &Path, conversation: &mut Conversation) -> io::Result<()> {
    let (database, id) = split(path).ok_or_else(|| not_a_session(path))?;
    let connection = open(&database)?;
    let session = connection
        .query_row(
            "SELECT directory, title, time_created, time_updated FROM session WHERE id = ?1",
            [&id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .optional()
        .map_err(io::Error::other)?
        .ok_or_else(|| not_a_session(path))?;
    let (directory, title, created, updated) = session;
    conversation.session_id = id.clone();
    if let Some(directory) = directory.filter(|d| !d.is_empty()) {
        conversation.project = Some(directory.into());
    }
    if let Some(title) = title.filter(|t| !is_placeholder_title(t)) {
        conversation.recorded_title = Some(Title {
            text: one_line(&title, 200),
            source: TitleSource::Agent,
        });
    }
    for at in [created, updated].into_iter().flatten() {
        if let Some(at) = chrono::DateTime::from_timestamp_millis(at) {
            conversation.saw_time(at);
        }
    }

    let mut parts = connection
        .prepare("SELECT message_id, data FROM part WHERE session_id = ?1 ORDER BY message_id, id")
        .map_err(io::Error::other)?;
    let mut by_message: Vec<(String, Value)> = Vec::new();
    for row in parts
        .query_map([&id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(io::Error::other)?
    {
        let (message, data) = row.map_err(io::Error::other)?;
        if let Ok(data) = serde_json::from_str(&data) {
            by_message.push((message, data));
        }
    }

    let mut messages = connection
        .prepare(
            "SELECT id, time_created, data FROM message WHERE session_id = ?1 \
             ORDER BY time_created, id",
        )
        .map_err(io::Error::other)?;
    for row in messages
        .query_map([&id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(io::Error::other)?
    {
        let (message_id, created, data) = row.map_err(io::Error::other)?;
        let Ok(data) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        let start = by_message.partition_point(|(m, _)| m.as_str() < message_id.as_str());
        let end = by_message.partition_point(|(m, _)| m.as_str() <= message_id.as_str());
        let at = created.and_then(chrono::DateTime::from_timestamp_millis);
        apply_message(conversation, &data, &by_message[start..end], at);
    }
    Ok(())
}

/// Whether a title is the placeholder OpenCode gives a session before it names it.
fn is_placeholder_title(title: &str) -> bool {
    let title = title.trim();
    title.is_empty() || title.starts_with("New session - ") || title.starts_with("Child session - ")
}

fn apply_message(
    conversation: &mut Conversation,
    message: &Value,
    parts: &[(String, Value)],
    at: Option<chrono::DateTime<chrono::Utc>>,
) {
    let role = match str_field(message, "role") {
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        _ => return,
    };
    if role == Role::Assistant
        && (message.get("summary").and_then(Value::as_bool) == Some(true)
            || str_field(message, "mode") == Some("compaction"))
    {
        return;
    }
    if let Some(at) = at {
        conversation.saw_time(at);
    }
    if role == Role::Assistant {
        if let Some(model) = first_str(message, &["modelID"]) {
            conversation.model = Some(model.to_string());
        }
    }
    if conversation.project.is_none() {
        if let Some(cwd) = message.pointer("/path/cwd").and_then(Value::as_str) {
            conversation.project = Some(cwd.into());
        }
    }
    for (_, part) in parts {
        match str_field(part, "type").unwrap_or_default() {
            "text" => {
                if part.get("synthetic").and_then(Value::as_bool) == Some(true)
                    || part.get("ignored").and_then(Value::as_bool) == Some(true)
                {
                    continue;
                }
                if let Some(text) = str_field(part, "text").filter(|t| !t.trim().is_empty()) {
                    let text = text.trim_end().to_string();
                    conversation.push_part(role, at, Part::Text { text });
                }
            }
            "file" => {
                if str_field(part, "mime").is_some_and(|mime| mime.starts_with("image/")) {
                    conversation.push_part(role, at, Part::Image);
                }
            }
            "tool" if role == Role::Assistant => {
                let name = str_field(part, "tool").unwrap_or("tool");
                let state = part.get("state").unwrap_or(&Value::Null);
                let Some(summary) = summarise(name, state) else {
                    continue;
                };
                conversation.push_part(
                    role,
                    at,
                    Part::Tool(ToolCall {
                        id: str_field(part, "callID").map(str::to_string),
                        name: name.to_string(),
                        summary,
                        failed: str_field(state, "status") == Some("error"),
                    }),
                );
            }
            _ => {}
        }
    }
}

fn summarise(name: &str, state: &Value) -> Option<String> {
    let input = state.get("input").unwrap_or(&Value::Null);
    let keys: &[&str] = match name {
        "invalid" => return None,
        "todowrite" | "todoread" => return Some("updated the task list".to_string()),
        "bash" => &["command"],
        "read" | "write" | "edit" | "multiedit" => &["filePath"],
        "glob" | "grep" => &["pattern"],
        "webfetch" => &["url"],
        "task" => &["description"],
        _ => &[
            "description",
            "command",
            "filePath",
            "path",
            "url",
            "query",
            "pattern",
        ],
    };
    let summary = first_str(input, keys)
        .or_else(|| first_str(state, &["title"]))
        .unwrap_or_default();
    Some(one_line(summary, SUMMARY_MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_paths_round_trip() {
        let path = session_path(Path::new("/data/opencode"), "ses_1");
        assert_eq!(path, Path::new("/data/opencode/opencode.db#ses_1"));
        assert_eq!(
            split(&path),
            Some((PathBuf::from("/data/opencode/opencode.db"), "ses_1".into()))
        );
        assert!(is_database_file(Path::new(
            "/data/opencode/opencode.db-wal"
        )));
        assert!(!is_database_file(&path));
        assert!(split(Path::new("/data/opencode/opencode.db")).is_none());
    }

    #[test]
    fn placeholder_titles_are_not_titles() {
        assert!(is_placeholder_title(
            "New session - 2026-01-02T10:00:00.000Z"
        ));
        assert!(!is_placeholder_title("Fix the login form"));
    }
}
