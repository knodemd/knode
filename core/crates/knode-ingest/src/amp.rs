//! Amp threads: `~/.local/share/amp/threads/T-<id>.json`.
//!
//! Unverified format: no Amp thread file was available to check this reader against, so it
//! follows the format described by people who inspected one, and reads it tolerantly. Each
//! thread is one JSON document, rewritten as it grows, and read whole when it changes:
//!
//! - `id`, `created` (milliseconds or RFC 3339), `title`;
//! - `env.initial.trees[].uri`: the project, as a `file://` URL;
//! - `messages[]`: `role` (`user`, `assistant`), `content` (a string or a list of blocks:
//!   `text`, `thinking`, `tool_use` with `id`, `name` and `input`, `tool_result` with
//!   `toolUseID` and `run.status`, `image`), and `meta.sentAt`.
//!
//! Thinking and tool output are left out; failed tool results mark their call as failed.

use serde_json::Value;

use crate::model::{Conversation, Part, Role, Title, TitleSource, ToolCall};
use crate::text::{SUMMARY_MAX, any_time, first_str, one_line, str_field};

pub(crate) fn apply_document(conversation: &mut Conversation, thread: &Value) {
    if let Some(id) = first_str(thread, &["id"]) {
        conversation.session_id = id.to_string();
    }
    if let Some(at) = any_time(thread.get("created")) {
        conversation.saw_time(at);
    }
    if let Some(title) = first_str(thread, &["title"]) {
        conversation.recorded_title = Some(Title {
            text: one_line(title, 200),
            source: TitleSource::Agent,
        });
    }
    if let Some(project) = project(thread) {
        conversation.project = Some(project.into());
    }
    for message in thread
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        apply_message(conversation, message);
    }
}

/// The local folder of the thread's first workspace tree.
fn project(thread: &Value) -> Option<String> {
    let trees = thread.pointer("/env/initial/trees")?.as_array()?;
    trees.iter().find_map(|tree| {
        let uri = str_field(tree, "uri")?;
        let path = uri.strip_prefix("file://")?;
        Some(percent_decode(path))
    })
}

fn apply_message(conversation: &mut Conversation, message: &Value) {
    let role = match str_field(message, "role") {
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        _ => return,
    };
    let at = message
        .get("meta")
        .and_then(|meta| any_time(meta.get("sentAt")))
        .or_else(|| any_time(message.get("timestamp")));
    if let Some(at) = at {
        conversation.saw_time(at);
    }
    if role == Role::Assistant {
        if let Some(model) = message
            .get("usage")
            .and_then(|usage| first_str(usage, &["model"]))
            .or_else(|| first_str(message, &["model"]))
        {
            conversation.model = Some(model.to_string());
        }
    }
    match message.get("content") {
        Some(Value::String(text)) => push_text(conversation, role, at, text),
        Some(Value::Array(blocks)) => {
            for block in blocks {
                match str_field(block, "type").unwrap_or_default() {
                    "text" => {
                        if let Some(text) = str_field(block, "text") {
                            push_text(conversation, role, at, text);
                        }
                    }
                    "image" => conversation.push_part(role, at, Part::Image),
                    "tool_use" if role == Role::Assistant => {
                        let name = str_field(block, "name").unwrap_or("tool");
                        let summary = summarise(name, block.get("input").unwrap_or(&Value::Null));
                        conversation.push_part(
                            role,
                            at,
                            Part::Tool(ToolCall {
                                id: str_field(block, "id").map(str::to_string),
                                name: name.to_string(),
                                summary,
                                failed: false,
                            }),
                        );
                    }
                    "tool_result" => {
                        let status = block.pointer("/run/status").and_then(Value::as_str);
                        let failed = matches!(status, Some("error" | "rejected-by-user"))
                            || block.get("is_error").and_then(Value::as_bool) == Some(true);
                        if failed {
                            if let Some(id) = first_str(block, &["toolUseID", "tool_use_id"]) {
                                conversation.mark_failed(id);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
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

fn summarise(name: &str, input: &Value) -> String {
    let keys: &[&str] = match name {
        "Bash" | "bash" => &["cmd", "command"],
        "todo_write" | "todo_read" => return "updated the task list".to_string(),
        _ => &[
            "description",
            "cmd",
            "command",
            "path",
            "filePath",
            "file_path",
            "url",
            "query",
            "pattern",
            "name",
        ],
    };
    first_str(input, keys)
        .map(|text| one_line(text, SUMMARY_MAX))
        .unwrap_or_default()
}

/// Decodes `%XX` escapes in a URL path.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(Ok(byte)) = text
                .get(i + 1..i + 3)
                .map(|hex| u8::from_str_radix(hex, 16))
            {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_file_urls() {
        assert_eq!(percent_decode("/work/my%20app"), "/work/my app");
        assert_eq!(percent_decode("/work/100%"), "/work/100%");
        assert_eq!(percent_decode("/work/%zz"), "/work/%zz");
    }
}
