//! Claude Code sessions: `~/.claude/projects/<encoded project>/<session id>.jsonl`.
//!
//! Each line is a JSON record with a `type`. The ones read here:
//!
//! - `user` and `assistant`: `message.content` is a string or a list of blocks (`text`,
//!   `thinking`, `tool_use`, `tool_result`, `image`). Records also carry `timestamp`, `cwd`,
//!   `gitBranch`, `sessionId`, `isSidechain` and `isMeta`. An assistant reply is written as one
//!   record per block, and tool results come back as `user` records.
//! - `custom-title` (`customTitle`), `ai-title` (`aiTitle`) and the older `summary` (`summary`):
//!   titles.
//!
//! Everything else (attachments, hooks, snapshots, usage, new record types) is ignored.

use serde_json::Value;

use crate::model::{Conversation, Part, Role, Title, TitleSource, ToolCall};
use crate::text::{SUMMARY_MAX, first_str, one_line, starts_with_tag, str_field, tag_content};

/// The model Claude Code names on records it writes itself, such as error notices.
const SYNTHETIC_MODEL: &str = "<synthetic>";

/// The directory name Claude Code uses for a project: every character other than an ASCII
/// letter or digit becomes `-`. The mapping cannot be reversed, so the project path is read from
/// the records, not from the directory name.
pub fn project_dir_name(project: &std::path::Path) -> String {
    project
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

pub(crate) fn apply(conversation: &mut Conversation, record: &Value) {
    if let Some(at) = crate::text::timestamp(record.get("timestamp")) {
        conversation.saw_time(at);
    }
    match str_field(record, "type").unwrap_or_default() {
        "custom-title" => set_title(conversation, record, "customTitle", TitleSource::User),
        "ai-title" => set_title(conversation, record, "aiTitle", TitleSource::Agent),
        "summary" => set_title(conversation, record, "summary", TitleSource::Agent),
        "user" => message(conversation, record, Role::User),
        "assistant" => message(conversation, record, Role::Assistant),
        _ => {}
    }
}

fn set_title(conversation: &mut Conversation, record: &Value, key: &str, source: TitleSource) {
    let Some(text) = first_str(record, &[key]) else {
        return;
    };
    let keep_existing = matches!(
        &conversation.recorded_title,
        Some(existing) if existing.source == TitleSource::User && source == TitleSource::Agent
    );
    if !keep_existing {
        conversation.recorded_title = Some(Title {
            text: one_line(text, 200),
            source,
        });
    }
}

fn message(conversation: &mut Conversation, record: &Value, role: Role) {
    if record.get("isSidechain").and_then(Value::as_bool) == Some(true)
        || record.get("isMeta").and_then(Value::as_bool) == Some(true)
        || record.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
    {
        return;
    }
    let Some(message) = record.get("message") else {
        return;
    };
    if role == Role::Assistant {
        match str_field(message, "model") {
            Some(SYNTHETIC_MODEL) => return,
            Some(model) if !model.is_empty() => conversation.model = Some(model.to_string()),
            _ => {}
        }
    }
    if conversation.project.is_none() {
        if let Some(cwd) = first_str(record, &["cwd"]) {
            conversation.project = Some(cwd.into());
        }
    }
    if conversation.git_branch.is_none() {
        if let Some(branch) = first_str(record, &["gitBranch"]) {
            conversation.git_branch = Some(branch.to_string());
        }
    }

    let at = crate::text::timestamp(record.get("timestamp"));
    match message.get("content") {
        Some(Value::String(text)) => {
            if let Some(text) = typed_text(text, role) {
                conversation.push_part(role, at, Part::Text { text });
            }
        }
        Some(Value::Array(blocks)) => {
            for block in blocks {
                match str_field(block, "type").unwrap_or_default() {
                    "text" => {
                        if let Some(text) =
                            str_field(block, "text").and_then(|t| typed_text(t, role))
                        {
                            conversation.push_part(role, at, Part::Text { text });
                        }
                    }
                    "image" => conversation.push_part(role, at, Part::Image),
                    "tool_use" if role == Role::Assistant => {
                        let name = str_field(block, "name").unwrap_or("tool").to_string();
                        let summary = summarise(&name, block.get("input").unwrap_or(&Value::Null));
                        let id = str_field(block, "id").map(str::to_string);
                        conversation.push_part(
                            role,
                            at,
                            Part::Tool(ToolCall {
                                id,
                                name,
                                summary,
                                failed: false,
                            }),
                        );
                    }
                    "tool_result"
                        if block.get("is_error").and_then(Value::as_bool) == Some(true) =>
                    {
                        if let Some(id) = str_field(block, "tool_use_id") {
                            conversation.mark_failed(id);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// The text worth keeping from a text block: `None` for empty text and for context Claude Code
/// injects, except slash commands, which are kept as `/name args`.
fn typed_text(text: &str, role: Role) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    if role == Role::User && starts_with_tag(text) {
        let name = tag_content(text, "command-name")?;
        let args = tag_content(text, "command-args").unwrap_or_default();
        return Some(if args.is_empty() {
            name.to_string()
        } else {
            format!("{name} {args}")
        });
    }
    Some(text.trim_end().to_string())
}

/// A one-line summary of a tool's input.
fn summarise(name: &str, input: &Value) -> String {
    let keys: &[&str] = match name {
        "Bash" => &["command"],
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => &["file_path", "notebook_path"],
        "Grep" | "Glob" => &["pattern"],
        "WebFetch" => &["url"],
        "WebSearch" => &["query"],
        "Agent" | "Task" => &["description", "subagent_type"],
        "Skill" => &["skill"],
        "TodoWrite" => return "updated the task list".to_string(),
        _ => &[
            "description",
            "command",
            "file_path",
            "path",
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn encodes_project_directories_like_claude_code() {
        assert_eq!(
            project_dir_name(Path::new("/Users/example/my.project")),
            "-Users-example-my-project"
        );
    }

    #[test]
    fn keeps_slash_commands_and_drops_injected_context() {
        let command = "<command-name>/review</command-name>\n<command-args>12</command-args>";
        assert_eq!(
            typed_text(command, Role::User).as_deref(),
            Some("/review 12")
        );
        assert_eq!(
            typed_text(
                "<local-command-stdout>ok</local-command-stdout>",
                Role::User
            ),
            None
        );
        assert_eq!(typed_text("  ", Role::User), None);
    }
}
