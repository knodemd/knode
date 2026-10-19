//! Codex sessions: `~/.codex/sessions/YYYY/MM/DD/rollout-<time>-<session id>.jsonl`.
//!
//! Each line is `{ "timestamp", "type", "payload" }`. The ones read here:
//!
//! - `session_meta`: `id`, `cwd`, `timestamp` and `git.branch`.
//! - `turn_context`: `model` and `cwd`.
//! - `response_item`: what the model saw and did. `message` items have a `role` (`user`,
//!   `assistant`, `developer`) and `content` blocks (`input_text`, `output_text`,
//!   `input_image`); `function_call` items carry `name` and JSON `arguments`;
//!   `custom_tool_call` items carry `name` and raw `input`; `web_search_call` items an `action`.
//!
//! Events (`event_msg`) repeat what the response items say and are ignored, as are tool outputs,
//! reasoning and unknown types. Older files without the `payload` wrapper are read too.

use serde_json::Value;

use crate::model::{Conversation, Part, Role, ToolCall};
use crate::text::{SUMMARY_MAX, first_str, one_line, starts_with_tag, str_field};

const WRAPPED: [&str; 5] = [
    "session_meta",
    "response_item",
    "event_msg",
    "turn_context",
    "compacted",
];

pub(crate) fn apply(conversation: &mut Conversation, record: &Value) {
    if record.get("record_type").is_some() {
        return;
    }
    let at = crate::text::timestamp(record.get("timestamp"));
    if let Some(at) = at {
        conversation.saw_time(at);
    }
    let (kind, payload) = match str_field(record, "type") {
        Some(kind) if WRAPPED.contains(&kind) => match record.get("payload") {
            Some(payload) => (kind, payload),
            None => return,
        },
        Some(_) => ("response_item", record),
        None if record.get("id").is_some() => ("session_meta", record),
        None => return,
    };
    match kind {
        "session_meta" => session_meta(conversation, payload),
        "turn_context" => {
            if let Some(model) = first_str(payload, &["model"]) {
                conversation.model = Some(model.to_string());
            }
            if conversation.project.is_none() {
                if let Some(cwd) = first_str(payload, &["cwd"]) {
                    conversation.project = Some(cwd.into());
                }
            }
        }
        "response_item" => response_item(conversation, payload, at),
        _ => {}
    }
}

fn session_meta(conversation: &mut Conversation, meta: &Value) {
    if let Some(id) = first_str(meta, &["id"]) {
        conversation.session_id = id.to_string();
    }
    if let Some(cwd) = first_str(meta, &["cwd"]) {
        conversation.project = Some(cwd.into());
    }
    if let Some(branch) = meta.get("git").and_then(|git| first_str(git, &["branch"])) {
        conversation.git_branch = Some(branch.to_string());
    }
    if let Some(at) = crate::text::timestamp(meta.get("timestamp")) {
        conversation.saw_time(at);
    }
}

fn response_item(
    conversation: &mut Conversation,
    item: &Value,
    at: Option<chrono::DateTime<chrono::Utc>>,
) {
    let tool = |name: &str, summary: String, id: Option<&str>| {
        Part::Tool(ToolCall {
            id: id.map(str::to_string),
            name: name.to_string(),
            summary,
            failed: false,
        })
    };
    match str_field(item, "type").unwrap_or_default() {
        "message" => message(conversation, item, at),
        "function_call" => {
            let name = str_field(item, "name").unwrap_or("tool");
            let arguments = match item.get("arguments") {
                Some(Value::String(text)) => serde_json::from_str(text).unwrap_or(Value::Null),
                Some(other) => other.clone(),
                None => Value::Null,
            };
            let Some(summary) = summarise_function(name, &arguments) else {
                return;
            };
            let part = tool(name, summary, str_field(item, "call_id"));
            conversation.push_part(Role::Assistant, at, part);
        }
        "custom_tool_call" => {
            let name = str_field(item, "name").unwrap_or("tool");
            let input = str_field(item, "input").unwrap_or_default();
            let summary = if name == "apply_patch" {
                patch_files(input)
            } else {
                one_line(input, SUMMARY_MAX)
            };
            let part = tool(name, summary, str_field(item, "call_id"));
            conversation.push_part(Role::Assistant, at, part);
        }
        "local_shell_call" => {
            let command = item
                .get("action")
                .map(|action| command_text(action.get("command")))
                .unwrap_or_default();
            let part = tool("shell", command, str_field(item, "call_id"));
            conversation.push_part(Role::Assistant, at, part);
        }
        "web_search_call" => {
            let action = item.get("action").unwrap_or(&Value::Null);
            let query = first_str(action, &["query", "url"])
                .map(str::to_string)
                .or_else(|| {
                    action
                        .get("queries")
                        .and_then(Value::as_array)
                        .and_then(|queries| queries.first())
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_default();
            let part = tool("web_search", one_line(&query, SUMMARY_MAX), None);
            conversation.push_part(Role::Assistant, at, part);
        }
        _ => {}
    }
}

fn message(
    conversation: &mut Conversation,
    item: &Value,
    at: Option<chrono::DateTime<chrono::Utc>>,
) {
    let role = match str_field(item, "role") {
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        _ => return,
    };
    let Some(blocks) = item.get("content").and_then(Value::as_array) else {
        return;
    };
    for block in blocks {
        match str_field(block, "type").unwrap_or_default() {
            "input_text" | "output_text" | "text" => {
                let Some(text) = str_field(block, "text") else {
                    continue;
                };
                if text.trim().is_empty() || is_injected(text) {
                    continue;
                }
                let text = text.trim_end().to_string();
                conversation.push_part(role, at, Part::Text { text });
            }
            "input_image" | "image" => conversation.push_part(role, at, Part::Image),
            _ => {}
        }
    }
}

/// Whether a user text block is context Codex adds rather than something a person typed.
fn is_injected(text: &str) -> bool {
    starts_with_tag(text)
        || text.trim_start().starts_with("# AGENTS.md instructions")
        || is_image_marker(text)
}

/// Whether text is one of the `<image ...>` and `</image>` lines Codex writes around an image.
fn is_image_marker(text: &str) -> bool {
    let text = text.trim();
    !text.contains('\n')
        && text.ends_with('>')
        && (text == "</image>" || text.starts_with("<image ") || text == "<image>")
}

/// A one-line summary of a function call, or `None` for calls that are only noise, such as
/// feeding input to a command that is already running.
fn summarise_function(name: &str, arguments: &Value) -> Option<String> {
    let summary = match name {
        "exec_command" | "shell" | "container.exec" | "shell_command" => {
            let command = arguments.get("cmd").or_else(|| arguments.get("command"));
            command_text(command)
        }
        "write_stdin" => return None,
        "update_plan" => "updated the plan".to_string(),
        "view_image" => first_str(arguments, &["path"])
            .unwrap_or_default()
            .to_string(),
        _ => first_str(
            arguments,
            &[
                "description",
                "cmd",
                "command",
                "path",
                "filePath",
                "url",
                "query",
                "name",
            ],
        )
        .unwrap_or_default()
        .to_string(),
    };
    Some(one_line(&summary, SUMMARY_MAX))
}

/// A command given as a string or as a list of arguments.
fn command_text(command: Option<&Value>) -> String {
    match command {
        Some(Value::String(text)) => one_line(text, SUMMARY_MAX),
        Some(Value::Array(parts)) => {
            let words: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
            one_line(&words.join(" "), SUMMARY_MAX)
        }
        _ => String::new(),
    }
}

/// The files a patch touches, comma separated.
fn patch_files(patch: &str) -> String {
    let files: Vec<&str> = patch
        .lines()
        .filter_map(|line| {
            ["*** Add File: ", "*** Update File: ", "*** Delete File: "]
                .iter()
                .find_map(|prefix| line.strip_prefix(prefix))
        })
        .map(str::trim)
        .collect();
    one_line(&files.join(", "), SUMMARY_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summarises_patches_by_file() {
        let patch = "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-x\n+y\n*** Add File: b.md\n+z\n*** End Patch";
        assert_eq!(patch_files(patch), "src/a.rs, b.md");
    }

    #[test]
    fn summarises_commands_given_as_lists() {
        let arguments = json!({ "command": ["bash", "-lc", "ls"] });
        assert_eq!(
            summarise_function("shell", &arguments).as_deref(),
            Some("bash -lc ls")
        );
        assert_eq!(summarise_function("write_stdin", &json!({})), None);
    }

    #[test]
    fn recognises_injected_context() {
        assert!(is_injected(
            "<environment_context>\n  <cwd>/x</cwd>\n</environment_context>"
        ));
        assert!(is_injected("# AGENTS.md instructions for /x"));
        assert!(is_injected("<image name=[Image #1]>"));
        assert!(is_injected("</image>"));
        assert!(!is_injected("Fix the failing test"));
    }
}
