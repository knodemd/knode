//! Cline and Roo Code tasks, in the two layouts Cline has written.
//!
//! **Task folders**, written by Roo Code and by Cline before its SDK (the VS Code extension's
//! classic bundle, which most windows still run, and the older Cline CLI and JetBrains plugin):
//! one folder per task, `<editor>/User/globalStorage/<extension>/tasks/<task id>/` for the
//! extensions and `~/.cline/data/tasks/<task id>/` for the Cline CLI. Roo Code is a fork of
//! Cline and writes the same files. Each task folder holds:
//!
//! - `ui_messages.json`: what the extension showed, a JSON array rewritten as the task runs, and
//!   read whole when it changes. Each message has `ts` (milliseconds), `type` (`say` or `ask`),
//!   `say` or `ask` (what kind of message it is), `text`, `images[]` and `partial` (still
//!   streaming). The first message is the task the person gave; `user_feedback` messages are
//!   the person's later replies; `text`, `completion_result` and `plan_completion_result` are
//!   the agent's answers, and `followup`, `plan_mode_respond` and `act_mode_respond` carry its
//!   question or answer in a JSON object; `tool` (a JSON object with `tool` and `path`, `regex`
//!   or `content`), `command` (the command line), `use_mcp_server` and `browser_action_launch`
//!   are tool calls. Reasoning, command output, API requests and checkpoints are left out.
//! - `api_conversation_history.json`: the messages sent to the model. Only its first part is
//!   searched, for the working directory the extension reports in `<environment_details>`.
//! - `history_item.json` (Roo Code): `workspace`, the project.
//! - `task_metadata.json` (Cline): `model_usage[].model_id`, the models used.
//!
//! When neither names the project, `../state/taskHistory.json` (Cline) is searched for the task's
//! `cwdOnTaskInitialization`.
//!
//! **SDK sessions**, written by the Cline SDK (the current Cline CLI, and the VS Code extension
//! windows that run on it): `~/.cline/data/sessions/<session id>/`, holding
//!
//! - `<session id>.messages.json`: the canonical record of the session (version `1` of its
//!   messages contract), rewritten as it runs and read whole when it changes: `sessionId`,
//!   `agent` (`lead` for the session itself) and `messages[]`, each with `role` (`user` or
//!   `assistant`), `ts` (milliseconds, on the agent's messages), `modelInfo.id` and `content[]`,
//!   blocks of `type` `text`, `thinking`, `tool_use` (`id`, `name`, `input`) and `tool_result`
//!   (`tool_use_id`, `is_error`). The person's text is wrapped in `<user_input mode="...">`
//!   (or `<user_command slash="...">` for a slash command), and can carry `<mode_notice>`
//!   elements and attached file contents, which are left out. Thinking and tool results are
//!   left out too.
//! - `<session id>.json`, the session's manifest: `started_at` and `ended_at` (RFC 3339), `cwd`
//!   and `workspace_root` (the project), `model`, and `metadata.title`.
//!
//! Subagents write their messages in the same folder under their own id, and are not captured.
//! A classic task resumed on the SDK keeps its id, so both layouts write the same node.
//!
//! These layouts follow Cline's and Roo Code's public source; they were not checked against
//! tasks on disk.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::model::{Conversation, Part, Role, Title, TitleSource, ToolCall};
use crate::text::{SUMMARY_MAX, any_time, first_str, one_line, str_field};

/// The file each task's conversation is read from.
pub(crate) const UI_MESSAGES: &str = "ui_messages.json";

/// The folder of Cline's data folder that holds SDK sessions.
pub(crate) const SDK_SESSIONS: &str = "sessions";

/// What the messages file of an SDK session is named after its session id.
pub(crate) const SDK_MESSAGES_SUFFIX: &str = ".messages.json";

/// How much of `api_conversation_history.json` is searched for the working directory.
const ENVIRONMENT_SEARCH: u64 = 256 * 1024;

/// Applies a whole `ui_messages.json` document of the task at `path`, or the messages document
/// of an SDK session. `project` is the project found by an earlier read, kept so the task's
/// other files are not searched again.
pub(crate) fn apply_document(
    conversation: &mut Conversation,
    messages: &Value,
    path: &Path,
    project: Option<PathBuf>,
) {
    let is_sdk = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(SDK_MESSAGES_SUFFIX));
    if is_sdk {
        apply_sdk_document(conversation, messages, path);
        return;
    }
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
        "text" | "completion_result" | "plan_completion_result" => {
            push_text(conversation, Role::Assistant, at, text)
        }
        "followup" => {
            let question = parse_object(text)
                .and_then(|object| first_str(&object, &["question"]).map(str::to_string))
                .unwrap_or_else(|| text.to_string());
            push_text(conversation, Role::Assistant, at, &question);
        }
        "plan_mode_respond" | "act_mode_respond" => {
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

// SDK sessions

/// Applies the messages document of an SDK session, and its manifest next to it.
fn apply_sdk_document(conversation: &mut Conversation, document: &Value, path: &Path) {
    if let Some(id) = first_str(document, &["sessionId"]) {
        conversation.session_id = id.to_string();
    }
    if let Some(manifest) = path
        .parent()
        .and_then(|dir| read_json(&dir.join(format!("{}.json", conversation.session_id))))
    {
        apply_manifest(conversation, &manifest);
    }
    for message in document
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        apply_sdk_message(conversation, message);
    }
}

fn apply_manifest(conversation: &mut Conversation, manifest: &Value) {
    if let Some(project) = first_str(manifest, &["cwd", "workspace_root"]) {
        conversation.project = Some(project.into());
    }
    if let Some(model) = first_str(manifest, &["model"]) {
        conversation.model = Some(model.to_string());
    }
    if let Some(title) = manifest
        .pointer("/metadata/title")
        .and_then(Value::as_str)
        .filter(|title| !title.trim().is_empty())
    {
        conversation.recorded_title = Some(Title {
            text: one_line(title, 200),
            source: TitleSource::Agent,
        });
    }
    for key in ["started_at", "ended_at"] {
        if let Some(at) = any_time(manifest.get(key)) {
            conversation.saw_time(at);
        }
    }
}

fn apply_sdk_message(conversation: &mut Conversation, message: &Value) {
    let role = match str_field(message, "role") {
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        _ => return,
    };
    let at = any_time(message.get("ts"));
    if let Some(at) = at {
        conversation.saw_time(at);
    }
    if role == Role::Assistant {
        if let Some(model) = message.pointer("/modelInfo/id").and_then(Value::as_str) {
            if !model.is_empty() {
                conversation.model = Some(model.to_string());
            }
        }
    }
    let single;
    let blocks: &[Value] = match message.get("content") {
        Some(Value::Array(blocks)) => blocks,
        Some(Value::String(text)) => {
            single = [serde_json::json!({"type": "text", "text": text})];
            &single
        }
        _ => return,
    };
    for block in blocks {
        match (role, str_field(block, "type")) {
            (Role::User, Some("text")) => {
                let text = user_input(str_field(block, "text").unwrap_or_default());
                push_text(conversation, Role::User, at, &text);
            }
            (Role::User, Some("image")) => conversation.push_part(Role::User, at, Part::Image),
            (Role::User, Some("tool_result")) => {
                if block.get("is_error").and_then(Value::as_bool) == Some(true) {
                    if let Some(id) = str_field(block, "tool_use_id") {
                        conversation.mark_failed(id);
                    }
                }
            }
            (Role::Assistant, Some("text")) => {
                push_text(
                    conversation,
                    Role::Assistant,
                    at,
                    str_field(block, "text").unwrap_or_default(),
                );
            }
            (Role::Assistant, Some("tool_use")) => {
                let name = first_str(block, &["name"]).unwrap_or("tool");
                let input = block.get("input").unwrap_or(&Value::Null);
                conversation.push_part(
                    Role::Assistant,
                    at,
                    Part::Tool(ToolCall {
                        id: first_str(block, &["id"]).map(str::to_string),
                        name: name.to_string(),
                        summary: one_line(&tool_summary(input), SUMMARY_MAX),
                        failed: false,
                    }),
                );
            }
            _ => {}
        }
    }
}

/// What the person typed, from a user text block of an SDK session: the content of its
/// `<user_input>` wrapper, or `/<command> <text>` for a `<user_command>`, without the
/// `<mode_notice>` elements and attached `<file_content>` blocks the SDK adds. Text that is
/// only such additions gives nothing.
fn user_input(text: &str) -> String {
    let mut text = remove_elements(text, "mode_notice");
    text = remove_elements(&text, "file_content");
    if let Some((attributes, content)) = element(&text, "user_command") {
        let slash = attribute(attributes, "slash").unwrap_or_default();
        let command = format!("/{} {}", slash.trim_start_matches('/'), content.trim());
        return command.trim().to_string();
    }
    if let Some((_, content)) = element(&text, "user_input") {
        return content.trim().to_string();
    }
    text.trim().to_string()
}

/// The attributes and content of the first `<tag ...>...</tag>` element in `text`.
fn element<'a>(text: &'a str, tag: &str) -> Option<(&'a str, &'a str)> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let start = text.find(&open)?;
    let rest = &text[start + open.len()..];
    if !rest.starts_with(['>', ' ']) {
        return None;
    }
    let tag_end = rest.find('>')?;
    let attributes = &rest[..tag_end];
    let body = &rest[tag_end + 1..];
    let end = body.find(&close).unwrap_or(body.len());
    Some((attributes, &body[..end]))
}

fn attribute<'a>(attributes: &'a str, key: &str) -> Option<&'a str> {
    let marker = format!("{key}=\"");
    let start = attributes.find(&marker)? + marker.len();
    let end = attributes[start..].find('"')? + start;
    Some(attributes[start..end].trim())
}

/// `text` without any `<tag ...>...</tag>` element, content included.
fn remove_elements(text: &str, tag: &str) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        if !rest[start + open.len()..].starts_with(['>', ' ']) {
            out.push_str(&rest[..start + open.len()]);
            rest = &rest[start + open.len()..];
            continue;
        }
        out.push_str(&rest[..start]);
        match rest[start..].find(&close) {
            Some(end) => rest = &rest[start + end + close.len()..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// A one-line summary of an SDK tool input: the path, command, query or URL it names, the
/// first of a list of them (`files`, `paths`, `commands`, `queries`, `requests`), or the input
/// itself when it is a string or a list of strings.
fn tool_summary(input: &Value) -> String {
    fn first(value: &Value) -> Option<String> {
        match value {
            Value::String(text) => Some(text.clone()),
            Value::Array(items) => items.iter().find_map(first),
            Value::Object(_) => {
                if let Some(text) = first_str(
                    value,
                    &[
                        "path",
                        "file_path",
                        "command",
                        "query",
                        "url",
                        "skill",
                        "question",
                    ],
                ) {
                    let mut text = text.to_string();
                    if let Some(args) = value.get("args").and_then(Value::as_array) {
                        for arg in args.iter().filter_map(Value::as_str) {
                            text.push(' ');
                            text.push_str(arg);
                        }
                    }
                    return Some(text);
                }
                ["files", "paths", "commands", "queries", "requests"]
                    .iter()
                    .find_map(|key| value.get(*key).and_then(first))
            }
            _ => None,
        }
    }
    first(input).unwrap_or_default()
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
