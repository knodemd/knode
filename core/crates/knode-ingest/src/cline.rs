//! Cline and Roo Code tasks: one folder per task in the extension's `globalStorage` folder,
//! `<editor>/User/globalStorage/<extension>/tasks/<task id>/`.
//!
//! Unverified format: no Cline or Roo Code task was available to check this reader against, so
//! it follows the extensions' public source and reads tolerantly. Roo Code is a fork of Cline
//! and writes the same files. Each task folder holds:
//!
//! - `ui_messages.json`: what the extension showed, a JSON array rewritten as the task runs, and
//!   read whole when it changes. Each message has `ts` (milliseconds), `type` (`say` or `ask`),
//!   `say` or `ask` (what kind of message it is), `text`, `images[]` and `partial` (still
//!   streaming). The first message is the task the person gave; `user_feedback` messages are
//!   the person's later replies; `text` and `completion_result` are the agent's answers;
//!   `tool` (a JSON object with `tool` and `path`, `regex` or `content`), `command` (the command
//!   line), `use_mcp_server` and `browser_action_launch` are tool calls. Reasoning, command
//!   output, API requests and checkpoints are left out.
//! - `api_conversation_history.json`: the messages sent to the model. Only its first part is
//!   searched, for the working directory the extension reports in `<environment_details>`.
//! - `history_item.json` (Roo Code): `workspace`, the project.
//! - `task_metadata.json` (Cline): `model_usage[].model_id`, the models used.
//!
//! When neither names the project, `../state/taskHistory.json` (Cline) is searched for the task's
//! `cwdOnTaskInitialization`.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::model::{Conversation, Part, Role, ToolCall};
use crate::text::{SUMMARY_MAX, any_time, first_str, one_line, str_field};

/// The file each task's conversation is read from.
pub(crate) const UI_MESSAGES: &str = "ui_messages.json";

/// How much of `api_conversation_history.json` is searched for the working directory.
const ENVIRONMENT_SEARCH: u64 = 256 * 1024;

/// Applies a whole `ui_messages.json` document of the task at `path`. `project` is the project
/// found by an earlier read, kept so the task's other files are not searched again.
pub(crate) fn apply_document(
    conversation: &mut Conversation,
    messages: &Value,
    path: &Path,
    project: Option<PathBuf>,
) {
    let task = path.parent().unwrap_or(Path::new(""));
    conversation.project = project.or_else(|| find_project(task, &conversation.session_id));
    conversation.model = model(task);
    let Some(messages) = messages.as_array() else {
        return;
    };
    for (index, message) in messages.iter().enumerate() {
        apply_message(conversation, message, index == 0);
    }
}

fn apply_message(conversation: &mut Conversation, message: &Value, first: bool) {
    if message.get("partial").and_then(Value::as_bool) == Some(true) {
        return;
    }
    let at = any_time(message.get("ts"));
    let kind = str_field(message, "say")
        .or_else(|| str_field(message, "ask"))
        .unwrap_or_default();
    let text = str_field(message, "text").unwrap_or_default();
    if first {
        push_user(conversation, message, at, text);
    } else {
        apply_kind(conversation, kind, message, at, text);
    }
    if let Some(at) = at {
        conversation.saw_time(at);
    }
}

fn apply_kind(
    conversation: &mut Conversation,
    kind: &str,
    message: &Value,
    at: Option<DateTime<Utc>>,
    text: &str,
) {
    match kind {
        "user_feedback" | "user_feedback_diff" => push_user(conversation, message, at, text),
        "text" | "completion_result" => push_text(conversation, Role::Assistant, at, text),
        "followup" => {
            let question = parse_object(text)
                .and_then(|object| first_str(&object, &["question"]).map(str::to_string))
                .unwrap_or_else(|| text.to_string());
            push_text(conversation, Role::Assistant, at, &question);
        }
        "plan_mode_respond" => {
            let response = parse_object(text)
                .and_then(|object| first_str(&object, &["response"]).map(str::to_string))
                .unwrap_or_else(|| text.to_string());
            push_text(conversation, Role::Assistant, at, &response);
        }
        "tool" => {
            let input = parse_object(text).unwrap_or(Value::Null);
            let name = first_str(&input, &["tool"]).unwrap_or("tool");
            let summary = first_str(
                &input,
                &[
                    "path",
                    "regex",
                    "filePattern",
                    "query",
                    "url",
                    "mode",
                    "command",
                ],
            )
            .unwrap_or_default();
            push_tool(conversation, at, name, summary);
        }
        "command" => push_tool(conversation, at, "execute_command", text),
        "use_mcp_server" => {
            let input = parse_object(text).unwrap_or(Value::Null);
            let name = first_str(&input, &["toolName", "uri"]).unwrap_or("use_mcp_server");
            let summary = first_str(&input, &["serverName"]).unwrap_or_default();
            push_tool(conversation, at, name, summary);
        }
        "browser_action_launch" => push_tool(conversation, at, "browser_action", text),
        "diff_error" | "clineignore_error" | "rooignore_error" => {
            mark_last_tool_failed(conversation)
        }
        _ => {}
    }
}

fn parse_object(text: &str) -> Option<Value> {
    serde_json::from_str::<Value>(text)
        .ok()
        .filter(Value::is_object)
}

fn push_user(
    conversation: &mut Conversation,
    message: &Value,
    at: Option<DateTime<Utc>>,
    text: &str,
) {
    push_text(conversation, Role::User, at, text);
    let images = message
        .get("images")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    for _ in 0..images {
        conversation.push_part(Role::User, at, Part::Image);
    }
}

fn push_text(conversation: &mut Conversation, role: Role, at: Option<DateTime<Utc>>, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    let text = text.trim_end().to_string();
    conversation.push_part(role, at, Part::Text { text });
}

fn push_tool(
    conversation: &mut Conversation,
    at: Option<DateTime<Utc>>,
    name: &str,
    summary: &str,
) {
    conversation.push_part(
        Role::Assistant,
        at,
        Part::Tool(ToolCall {
            id: None,
            name: name.to_string(),
            summary: one_line(summary, SUMMARY_MAX),
            failed: false,
        }),
    );
}

/// Marks the last tool call as failed, as when Cline reports that an edit did not apply.
fn mark_last_tool_failed(conversation: &mut Conversation) {
    let Some(last) = conversation.messages.last_mut() else {
        return;
    };
    if last.role != Role::Assistant {
        return;
    }
    if let Some(Part::Tool(call)) = last
        .parts
        .iter_mut()
        .rev()
        .find(|part| matches!(part, Part::Tool(_)))
    {
        call.failed = true;
    }
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// The last model the task used, from Cline's `task_metadata.json`.
fn model(task: &Path) -> Option<String> {
    let metadata = read_json(&task.join("task_metadata.json"))?;
    metadata
        .get("model_usage")?
        .as_array()?
        .iter()
        .rev()
        .find_map(|usage| first_str(usage, &["model_id"]))
        .map(str::to_string)
}

/// The folder the task ran in.
fn find_project(task: &Path, id: &str) -> Option<PathBuf> {
    if let Some(workspace) = read_json(&task.join("history_item.json"))
        .as_ref()
        .and_then(|item| first_str(item, &["workspace", "cwdOnTaskInitialization"]))
    {
        return Some(workspace.into());
    }
    if let Some(directory) = environment_directory(&task.join("api_conversation_history.json")) {
        return Some(directory);
    }
    let history = task
        .parent()?
        .parent()?
        .join("state")
        .join("taskHistory.json");
    let history = read_json(&history)?;
    history.as_array()?.iter().find_map(|item| {
        if str_field(item, "id") != Some(id) {
            return None;
        }
        first_str(item, &["cwdOnTaskInitialization", "workspace"]).map(PathBuf::from)
    })
}

/// The working directory reported in the first `<environment_details>` of the task, as
/// `# Current Working Directory (<path>) Files` (Cline) or `# Current Workspace Directory
/// (<path>) Files` (Roo Code).
fn environment_directory(path: &Path) -> Option<PathBuf> {
    let mut head = Vec::new();
    File::open(path)
        .ok()?
        .take(ENVIRONMENT_SEARCH)
        .read_to_end(&mut head)
        .ok()?;
    let head = String::from_utf8_lossy(&head);
    for marker in [
        "# Current Working Directory (",
        "# Current Workspace Directory (",
    ] {
        let Some(start) = head.find(marker).map(|at| at + marker.len()) else {
            continue;
        };
        let rest = &head[start..];
        let end = rest.find(") Files").or_else(|| rest.find(')'))?;
        let directory = rest[..end].trim();
        if !directory.is_empty() {
            // The text sits in a JSON string: undo the escapes a path can carry.
            return Some(directory.replace("\\\\", "\\").replace("\\/", "/").into());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_working_directory_in_the_environment() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("api_conversation_history.json");
        fs::write(
            &file,
            r#"[{"role":"user","content":[{"type":"text","text":"<environment_details>\n# Current Working Directory (/Users/example/project) Files\nsrc/\n</environment_details>"}]}]"#,
        )
        .unwrap();
        assert_eq!(
            environment_directory(&file),
            Some(PathBuf::from("/Users/example/project"))
        );
        fs::write(&file, "[]").unwrap();
        assert_eq!(environment_directory(&file), None);
    }
}
