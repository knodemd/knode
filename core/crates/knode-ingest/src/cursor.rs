//! Cursor chats and agent sessions ("composers"), in the SQLite database `state.vscdb` in
//! Cursor's `User/globalStorage` folder.
//!
//! The database is read as described in [`crate::vscdb`]: read-only, without locks that get in
//! Cursor's way, and never copied. Its `cursorDiskKV` table holds one JSON value per key:
//!
//! - `composerData:<composer id>`: one conversation, with `composerId`, `name` (the title),
//!   `createdAt` and `lastUpdatedAt` (milliseconds), `modelConfig.modelName`,
//!   `workspaceIdentifier.uri.fsPath` (the project), `trackedGitRepos[]` (`repoPath`,
//!   `branches[]` with `branchName` and `lastInteractionAt`), `isBestOfNSubcomposer`, and
//!   `fullConversationHeadersOnly[]`: the turns in order, each a `bubbleId` and a `type`
//!   (`1` the person, `2` the agent). Older versions keep the turns inline in `conversation[]`.
//! - `bubbleId:<composer id>:<bubble id>`: one turn, with `type`, `text`, `createdAt`,
//!   `modelInfo.modelName`, `images[]`, and for a tool call `toolFormerData`: `toolCallId`,
//!   `name`, `rawArgs` (the input, as a JSON string), `params` and `status`.
//!
//! Newer versions also keep a `composerHeaders` table (`composerId`, `workspaceId`,
//! `isSubagent`, and a `value` with `workspaceIdentifier`), used to leave out subagents and to
//! find the project; the folder of `workspaceId` in `User/workspaceStorage` names it otherwise.
//!
//! The tables, the `composerData` fields and the `composerHeaders` table were checked against a
//! real Cursor database (structure only). The turn rows (`bubbleId:`) could not be: their
//! format follows public descriptions, and is read tolerantly. Thinking, tool output, code
//! blocks and file contents are left out.
//!
//! A conversation is read again whole when its `composerData` value changes, which happens with
//! every new turn and when a turn finishes; its version is a hash of that value.

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use crate::discover::SessionFile;
use crate::model::{Agent, Conversation, Part, Role, Title, TitleSource, ToolCall};
use crate::text::{SUMMARY_MAX, any_time, first_str, one_line, str_field};
use crate::vscdb::{self, DATABASE};

const COMPOSER: &str = "composerData:";
/// The key just after every `composerData:` key (`;` follows `:`).
const COMPOSER_END: &str = "composerData;";
const BUBBLE: &str = "bubbleId:";

/// The conversations in the database in `root` (Cursor's `User/globalStorage` folder) that
/// have at least one turn, without subagents. A missing database gives none.
pub(crate) fn list(root: &Path) -> io::Result<Vec<SessionFile>> {
    let connection = match vscdb::open(&root.join(DATABASE)) {
        Ok(connection) => connection,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    if !vscdb::has_table(&connection, "cursorDiskKV")? {
        return Ok(Vec::new());
    }
    let subagents = subagents(&connection)?;
    let mut statement = connection
        .prepare(
            "SELECT key, value, \
               coalesce(json_extract(v, '$.lastUpdatedAt'), json_extract(v, '$.createdAt')) \
             FROM (SELECT key, value, CAST(value AS TEXT) AS v FROM cursorDiskKV \
                   WHERE key >= ?1 AND key < ?2) \
             WHERE json_valid(v) \
               AND (coalesce(json_array_length(v, '$.fullConversationHeadersOnly'), 0) > 0 \
                    OR coalesce(json_array_length(v, '$.conversation'), 0) > 0) \
               AND coalesce(json_extract(v, '$.isBestOfNSubcomposer'), 0) = 0",
        )
        .map_err(io::Error::other)?;
    let rows = statement
        .query_map([COMPOSER, COMPOSER_END], |row| {
            Ok((
                row.get::<_, String>(0)?,
                vscdb::bytes(row, 1),
                row.get::<_, Option<i64>>(2).ok().flatten(),
            ))
        })
        .map_err(io::Error::other)?;
    let mut files = Vec::new();
    for row in rows {
        let (key, value, updated) = row.map_err(io::Error::other)?;
        let id = &key[COMPOSER.len()..];
        if id.is_empty() || subagents.contains(id) {
            continue;
        }
        files.push(SessionFile {
            agent: Agent::Cursor,
            path: vscdb::session_path(root, id),
            session_id: id.to_string(),
            len: vscdb::hash(&value),
            modified: updated
                .and_then(|ms| u64::try_from(ms).ok())
                .map(|ms| UNIX_EPOCH + Duration::from_millis(ms)),
        });
    }
    Ok(files)
}

/// The composers the `composerHeaders` table marks as subagents.
fn subagents(connection: &Connection) -> io::Result<BTreeSet<String>> {
    if !vscdb::has_table(connection, "composerHeaders")? {
        return Ok(BTreeSet::new());
    }
    let mut statement = connection
        .prepare("SELECT composerId FROM composerHeaders WHERE isSubagent = 1")
        .map_err(io::Error::other)?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(io::Error::other)?
        .collect::<Result<_, _>>()
        .map_err(io::Error::other)?;
    Ok(ids)
}

fn not_a_session(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("{} is not a Cursor conversation", path.display()),
    )
}

fn composer(connection: &Connection, id: &str) -> io::Result<Option<Vec<u8>>> {
    connection
        .query_row(
            "SELECT value FROM cursorDiskKV WHERE key = ?1",
            [format!("{COMPOSER}{id}")],
            |row| Ok(vscdb::bytes(row, 0)),
        )
        .optional()
        .map_err(io::Error::other)
        .map(|value| value.filter(|value| !value.is_empty()))
}

/// The version of a conversation, or `None` if it is gone.
pub(crate) fn version(path: &Path) -> io::Result<Option<u64>> {
    let (database, id) = vscdb::split(path).ok_or_else(|| not_a_session(path))?;
    let connection = vscdb::open(&database)?;
    if !vscdb::has_table(&connection, "cursorDiskKV")? {
        return Ok(None);
    }
    Ok(composer(&connection, &id)?.map(|value| vscdb::hash(&value)))
}

/// Reads the whole conversation at `path` into `conversation`, which should be empty, and
/// returns its version.
pub(crate) fn read(path: &Path, conversation: &mut Conversation) -> io::Result<u64> {
    let (database, id) = vscdb::split(path).ok_or_else(|| not_a_session(path))?;
    let connection = vscdb::open(&database)?;
    if !vscdb::has_table(&connection, "cursorDiskKV")? {
        return Err(not_a_session(path));
    }
    let raw = composer(&connection, &id)?.ok_or_else(|| not_a_session(path))?;
    let version = vscdb::hash(&raw);
    let data: Value = serde_json::from_slice(&raw)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    conversation.session_id = id.clone();

    if let Some(name) = first_str(&data, &["name"]) {
        conversation.recorded_title = Some(Title {
            text: one_line(name, 200),
            source: TitleSource::Agent,
        });
    }
    for key in ["createdAt", "lastUpdatedAt"] {
        if let Some(at) = any_time(data.get(key)) {
            conversation.saw_time(at);
        }
    }
    if let Some(model) = data
        .pointer("/modelConfig/modelName")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty() && *model != "default")
    {
        conversation.model = Some(model.to_string());
    }
    conversation.git_branch = latest_branch(&data);
    conversation.project = project(&connection, &database, &id, &data)?;

    let bubbles = bubbles(&connection, &id)?;
    if let Some(headers) = data
        .get("fullConversationHeadersOnly")
        .and_then(Value::as_array)
        .filter(|headers| !headers.is_empty())
    {
        for header in headers {
            let Some(bubble) = str_field(header, "bubbleId").and_then(|b| bubbles.get(b)) else {
                continue;
            };
            let kind = header.get("type").and_then(Value::as_i64);
            apply_bubble(conversation, bubble, kind);
        }
    } else if let Some(inline) = data.get("conversation").and_then(Value::as_array) {
        for bubble in inline {
            apply_bubble(conversation, bubble, None);
        }
    }
    Ok(version)
}

/// Every turn row of a composer, by bubble id. The key range stays on the table's index.
fn bubbles(connection: &Connection, id: &str) -> io::Result<HashMap<String, Value>> {
    let prefix = format!("{BUBBLE}{id}:");
    let end = format!("{BUBBLE}{id};");
    let mut statement = connection
        .prepare("SELECT key, value FROM cursorDiskKV WHERE key >= ?1 AND key < ?2")
        .map_err(io::Error::other)?;
    let rows = statement
        .query_map([&prefix, &end], |row| {
            Ok((row.get::<_, String>(0)?, vscdb::bytes(row, 1)))
        })
        .map_err(io::Error::other)?;
    let mut bubbles = HashMap::new();
    for row in rows {
        let (key, value) = row.map_err(io::Error::other)?;
        if let Ok(value) = serde_json::from_slice::<Value>(&value) {
            bubbles.insert(key[prefix.len()..].to_string(), value);
        }
    }
    Ok(bubbles)
}

/// The branch of the tracked repository the conversation last touched.
fn latest_branch(data: &Value) -> Option<String> {
    let repos = data.get("trackedGitRepos")?.as_array()?;
    repos
        .iter()
        .filter_map(|repo| repo.get("branches")?.as_array())
        .flatten()
        .filter_map(|branch| {
            let name = str_field(branch, "branchName").filter(|n| !n.is_empty())?;
            let at = branch
                .get("lastInteractionAt")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            Some((at, name))
        })
        .max_by_key(|(at, _)| *at)
        .map(|(_, name)| name.to_string())
}

/// The folder the conversation was held in: the workspace in the composer or its header, the
/// first tracked repository, or the folder of the header's workspace in `workspaceStorage`.
fn project(
    connection: &Connection,
    database: &Path,
    id: &str,
    data: &Value,
) -> io::Result<Option<PathBuf>> {
    if let Some(path) = workspace_path(data) {
        return Ok(Some(path));
    }
    let header = if vscdb::has_table(connection, "composerHeaders")? {
        connection
            .query_row(
                "SELECT workspaceId, value FROM composerHeaders WHERE composerId = ?1",
                [id],
                |row| Ok((row.get::<_, Option<String>>(0)?, vscdb::bytes(row, 1))),
            )
            .optional()
            .map_err(io::Error::other)?
    } else {
        None
    };
    if let Some((_, value)) = &header {
        if let Some(path) = serde_json::from_slice::<Value>(value)
            .ok()
            .and_then(|value| workspace_path(&value))
        {
            return Ok(Some(path));
        }
    }
    if let Some(repo) = data
        .get("trackedGitRepos")
        .and_then(Value::as_array)
        .and_then(|repos| repos.iter().find_map(|r| str_field(r, "repoPath")))
        .filter(|repo| !repo.is_empty())
    {
        return Ok(Some(repo.into()));
    }
    let workspace = header
        .and_then(|(workspace, _)| workspace)
        .filter(|w| !w.is_empty() && !w.contains(['/', '\\', '.']));
    Ok(workspace.and_then(|workspace| {
        let storage = database.parent()?.parent()?.join("workspaceStorage");
        vscdb::workspace_folder(&storage.join(workspace))
    }))
}

fn workspace_path(value: &Value) -> Option<PathBuf> {
    let identifier = value.get("workspaceIdentifier")?;
    identifier
        .pointer("/uri/fsPath")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let uri = identifier
                .pointer("/uri/external")
                .and_then(Value::as_str)?;
            vscdb::file_uri_path(uri)
        })
}

fn apply_bubble(conversation: &mut Conversation, bubble: &Value, kind: Option<i64>) {
    let role = match bubble.get("type").and_then(Value::as_i64).or(kind) {
        Some(1) => Role::User,
        Some(2) => Role::Assistant,
        _ => return,
    };
    let at: Option<DateTime<Utc>> = any_time(bubble.get("createdAt"))
        .or_else(|| any_time(bubble.pointer("/timingInfo/clientStartTime")));
    let text = str_field(bubble, "text")
        .filter(|text| !text.trim().is_empty())
        .map(|text| text.trim_end().to_string());
    let images = bubble
        .get("images")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let tool = if role == Role::Assistant {
        bubble.get("toolFormerData").filter(|t| t.is_object())
    } else {
        None
    };
    if text.is_none() && images == 0 && tool.is_none() {
        return;
    }
    if let Some(at) = at {
        conversation.saw_time(at);
    }
    if role == Role::Assistant {
        if let Some(model) = bubble
            .pointer("/modelInfo/modelName")
            .and_then(Value::as_str)
            .filter(|model| !model.is_empty() && *model != "default")
        {
            conversation.model = Some(model.to_string());
        }
    }
    if let Some(text) = text {
        conversation.push_part(role, at, Part::Text { text });
    }
    for _ in 0..images {
        conversation.push_part(role, at, Part::Image);
    }
    if let Some(tool) = tool {
        let name = first_str(tool, &["name"]).unwrap_or("tool");
        conversation.push_part(
            role,
            at,
            Part::Tool(ToolCall {
                id: first_str(tool, &["toolCallId"]).map(str::to_string),
                name: name.to_string(),
                summary: summarise(name, tool),
                failed: str_field(tool, "status") == Some("error"),
            }),
        );
    }
}

/// The input of a tool call: `rawArgs` or `params`, either a JSON string or an object.
fn arguments(tool: &Value) -> Value {
    for key in ["rawArgs", "params"] {
        match tool.get(key) {
            Some(Value::String(text)) => {
                if let Ok(value @ Value::Object(_)) = serde_json::from_str(text) {
                    return value;
                }
            }
            Some(value @ Value::Object(_)) => return value.clone(),
            _ => {}
        }
    }
    Value::Null
}

fn summarise(name: &str, tool: &Value) -> String {
    if name.starts_with("todo_") {
        return "updated the task list".to_string();
    }
    let input = arguments(tool);
    let summary = first_str(
        &input,
        &[
            "command",
            "target_file",
            "targetFile",
            "relative_workspace_path",
            "file_path",
            "filePath",
            "path",
            "query",
            "pattern",
            "glob_pattern",
            "search_term",
            "url",
            "explanation",
        ],
    )
    .unwrap_or_default();
    one_line(summary, SUMMARY_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn finds_the_latest_branch() {
        let data = json!({"trackedGitRepos": [{"repoPath": "/p", "branches": [
            {"branchName": "main", "lastInteractionAt": 1},
            {"branchName": "feature", "lastInteractionAt": 5},
            {"branchName": "", "lastInteractionAt": 9}
        ]}]});
        assert_eq!(latest_branch(&data).as_deref(), Some("feature"));
        assert_eq!(latest_branch(&json!({})), None);
    }

    #[test]
    fn summarises_tool_input_given_as_text_or_object() {
        let raw = json!({"name": "run_terminal_cmd", "rawArgs": "{\"command\": \"cargo test\"}"});
        assert_eq!(summarise("run_terminal_cmd", &raw), "cargo test");
        let params = json!({"name": "read_file", "params": {"target_file": "src/lib.rs"}});
        assert_eq!(summarise("read_file", &params), "src/lib.rs");
        assert_eq!(summarise("todo_write", &json!({})), "updated the task list");
        assert_eq!(summarise("unknown", &json!({"rawArgs": "not json"})), "");
    }
}
