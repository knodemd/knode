//! Gemini CLI sessions: `~/.gemini/tmp/<project>/chats/session-<time>-<short id>.jsonl`.
//!
//! Gemini CLI writes one JSON record per line:
//!
//! - first, the session: `sessionId`, `projectHash`, `startTime`, `lastUpdated` and `kind`
//!   (`main`, or `subagent` for the sessions of subagents, which live in a folder of their own);
//! - messages: `id`, `timestamp`, `type` (`user`, `gemini`, `info`, `error`) and `content`, a
//!   string or a list of parts (`text`, `inlineData`, `functionResponse`). Gemini messages also
//!   carry `model`, `thoughts` and `toolCalls` (`id`, `name`, `args`, `status`). A message is
//!   written again under the same `id` as it grows, and the newest copy replaces the older one;
//! - `{"$set": {...}}`: metadata updates, such as `summary` and `lastUpdated`;
//! - `{"$rewindTo": "<id>"}`: the person rewound the conversation to before that message.
//!
//! Older versions wrote the whole session as one JSON document (`session-*.json`) with the
//! session fields and a `messages` list; it is read whole each time it changes.
//!
//! The project folder is in the `.project_root` file next to `chats` (newer versions name the
//! folder after the project; older ones used a hash of its path and have no such file).
//! Thoughts, tool output, info and error messages are left out.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::model::{Conversation, Part, Role, Title, TitleSource, ToolCall};
use crate::text::{SUMMARY_MAX, first_str, one_line, str_field};

/// Applies one line of a `.jsonl` session.
pub(crate) fn apply(conversation: &mut Conversation, record: &Value) {
    if let Some(id) = str_field(record, "$rewindTo") {
        conversation.rewind(id);
        return;
    }
    if let Some(set) = record.get("$set") {
        metadata(conversation, set);
        return;
    }
    if record.get("type").is_some() && record.get("id").is_some() {
        let id = str_field(record, "id").unwrap_or_default().to_string();
        if conversation.begin_record(&id) {
            message(conversation, record);
        }
        return;
    }
    metadata(conversation, record);
}

/// Applies a whole `.json` session.
pub(crate) fn apply_document(conversation: &mut Conversation, document: &Value) {
    metadata(conversation, document);
    if let Some(messages) = document.get("messages").and_then(Value::as_array) {
        for record in messages {
            message(conversation, record);
        }
    }
}

/// The project folder recorded next to the session's `chats` folder, if any.
pub(crate) fn project_root(session: &Path) -> Option<PathBuf> {
    let folder = session.parent()?.parent()?;
    let text = fs::read_to_string(folder.join(".project_root")).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| PathBuf::from(text))
}

fn metadata(conversation: &mut Conversation, record: &Value) {
    if let Some(id) = first_str(record, &["sessionId"]) {
        conversation.session_id = id.to_string();
    }
    for key in ["startTime", "lastUpdated"] {
        if let Some(at) = crate::text::timestamp(record.get(key)) {
            conversation.saw_time(at);
        }
    }
    if let Some(summary) = first_str(record, &["summary"]) {
        if !matches!(&conversation.recorded_title, Some(t) if t.source == TitleSource::User) {
            conversation.recorded_title = Some(Title {
                text: one_line(summary, 200),
                source: TitleSource::Agent,
            });
        }
    }
}

fn message(conversation: &mut Conversation, record: &Value) {
    let at = crate::text::timestamp(record.get("timestamp"));
    if let Some(at) = at {
        conversation.saw_time(at);
    }
    let role = match str_field(record, "type") {
        Some("user") => Role::User,
        Some("gemini") => Role::Assistant,
        _ => return,
    };
    if role == Role::Assistant {
        if let Some(model) = first_str(record, &["model"]) {
            conversation.model = Some(model.to_string());
        }
    }
    // What the person saw, when it differs from what was sent (for example `@file` expanded).
    let content = match record.get("displayContent") {
        Some(Value::Array(parts)) if !parts.is_empty() => record.get("displayContent"),
        _ => record.get("content"),
    };
    match content {
        Some(Value::String(text)) => push_text(conversation, role, at, text),
        Some(Value::Array(parts)) => {
            for part in parts {
                if let Some(text) = str_field(part, "text") {
                    push_text(conversation, role, at, text);
                } else if part.get("inlineData").is_some() || part.get("fileData").is_some() {
                    conversation.push_part(role, at, Part::Image);
                }
            }
        }
        _ => {}
    }
    if role == Role::Assistant {
        for call in record
            .get("toolCalls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let name = str_field(call, "name").unwrap_or("tool");
            let Some(summary) = summarise(name, call.get("args").unwrap_or(&Value::Null)) else {
                continue;
            };
            let part = Part::Tool(ToolCall {
                id: str_field(call, "id").map(str::to_string),
                name: name.to_string(),
                summary,
                failed: str_field(call, "status") == Some("error"),
            });
            conversation.push_part(role, at, part);
        }
    }
}

fn push_text(
    conversation: &mut Conversation,
    role: Role,
    at: Option<chrono::DateTime<chrono::Utc>>,
    text: &str,
) {
    if text.trim().is_empty() {
        return;
    }
    let text = text.trim_end().to_string();
    conversation.push_part(role, at, Part::Text { text });
}

/// A one-line summary of a tool call, or `None` for calls that are only bookkeeping.
fn summarise(name: &str, args: &Value) -> Option<String> {
    let keys: &[&str] = match name {
        "update_topic" => return None,
        "run_shell_command" => &["command"],
        "read_file" | "write_file" | "replace" | "read_many_files" => &["file_path", "paths"],
        "list_directory" => &["dir_path", "path"],
        "glob" | "grep_search" | "search_file_content" => &["pattern"],
        "google_web_search" => &["query"],
        "web_fetch" => &["url", "prompt"],
        "write_todos" => return Some("updated the task list".to_string()),
        _ => &[
            "description",
            "command",
            "file_path",
            "filePath",
            "path",
            "dir_path",
            "url",
            "query",
            "pattern",
            "objective",
            "agent_name",
        ],
    };
    Some(
        first_str(args, keys)
            .map(|text| one_line(text, SUMMARY_MAX))
            .unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Agent;
    use serde_json::json;

    fn user(id: &str, text: &str) -> Value {
        json!({"id": id, "timestamp": "2026-01-02T10:00:00Z", "type": "user", "content": text})
    }

    fn reply(id: &str, text: &str) -> Value {
        json!({"id": id, "timestamp": "2026-01-02T10:00:05Z", "type": "gemini", "content": text, "model": "gemini-test"})
    }

    #[test]
    fn a_message_written_again_replaces_the_older_copy() {
        let mut conversation = Conversation::new(Agent::GeminiCli, "s");
        apply(&mut conversation, &user("u1", "Hello"));
        apply(&mut conversation, &reply("g1", "Thinking"));
        apply(&mut conversation, &reply("g1", "Thinking done"));
        assert_eq!(conversation.messages.len(), 2);
        assert_eq!(conversation.messages[1].text(), "Thinking done");
        assert_eq!(conversation.model.as_deref(), Some("gemini-test"));
    }

    #[test]
    fn rewinding_takes_back_later_messages() {
        let mut conversation = Conversation::new(Agent::GeminiCli, "s");
        apply(&mut conversation, &user("u1", "First"));
        apply(&mut conversation, &reply("g1", "One"));
        apply(&mut conversation, &user("u2", "Second"));
        apply(&mut conversation, &reply("g2", "Two"));
        apply(&mut conversation, &json!({"$rewindTo": "u2"}));
        assert_eq!(conversation.messages.len(), 2);
        apply(&mut conversation, &user("u3", "Instead"));
        assert_eq!(conversation.messages[2].text(), "Instead");
    }

    #[test]
    fn leaves_out_bookkeeping_tools() {
        assert_eq!(summarise("update_topic", &json!({"title": "x"})), None);
        assert_eq!(
            summarise("run_shell_command", &json!({"command": "ls -la"})).as_deref(),
            Some("ls -la")
        );
    }
}
