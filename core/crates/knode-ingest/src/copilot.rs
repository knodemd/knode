//! GitHub Copilot CLI sessions: `~/.copilot/session-state/<session id>/events.jsonl`.
//!
//! Each line is an event `{ "id", "parentId", "timestamp", "type", "data" }`. The ones read here:
//!
//! - `session.start` and `session.resume`: `sessionId`, `selectedModel` and `context` (`cwd`,
//!   `gitRoot`, `branch`);
//! - `session.model_change` (`newModel`) and `session.title_changed` (`title`);
//! - `user.message`: `content` as typed (`transformedContent` is what the model saw, with context
//!   added, and is not kept); messages with a `source` such as `skill-…` were injected and are
//!   left out;
//! - `assistant.message`: `content` and `toolRequests` (`toolCallId`, `name`, `arguments`).
//!   Messages with a `parentToolCallId` belong to a subagent and are left out;
//! - `tool.execution_complete`: `success`, used to mark failed calls.
//!
//! Streaming deltas, reasoning, usage, hooks, permissions and other events are ignored.

use serde_json::Value;

use crate::model::{Conversation, Part, Role, Title, TitleSource, ToolCall};
use crate::text::{SUMMARY_MAX, first_str, one_line, str_field};

pub(crate) fn apply(conversation: &mut Conversation, record: &Value) {
    let at = crate::text::timestamp(record.get("timestamp"));
    if let Some(at) = at {
        conversation.saw_time(at);
    }
    let data = record.get("data").unwrap_or(&Value::Null);
    match str_field(record, "type").unwrap_or_default() {
        "session.start" | "session.resume" => {
            if let Some(id) = first_str(data, &["sessionId"]) {
                conversation.session_id = id.to_string();
            }
            if let Some(model) = first_str(data, &["selectedModel"]) {
                conversation.model = Some(model.to_string());
            }
            if let Some(context) = data.get("context") {
                context_changed(conversation, context);
            }
        }
        "session.context_changed" => context_changed(conversation, data),
        "session.model_change" => {
            if let Some(model) = first_str(data, &["newModel"]) {
                conversation.model = Some(model.to_string());
            }
        }
        "session.title_changed" => {
            if let Some(title) = first_str(data, &["title"]) {
                conversation.recorded_title = Some(Title {
                    text: one_line(title, 200),
                    source: TitleSource::Agent,
                });
            }
        }
        "user.message" => {
            if str_field(data, "source").is_some_and(|source| source != "user") {
                return;
            }
            if let Some(text) = str_field(data, "content").filter(|t| !t.trim().is_empty()) {
                let text = text.trim_end().to_string();
                conversation.push_part(Role::User, at, Part::Text { text });
            }
        }
        "assistant.message" => {
            if data.get("parentToolCallId").is_some_and(|v| !v.is_null()) {
                return;
            }
            if let Some(text) = str_field(data, "content").filter(|t| !t.trim().is_empty()) {
                let text = text.trim_end().to_string();
                conversation.push_part(Role::Assistant, at, Part::Text { text });
            }
            for request in data
                .get("toolRequests")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let name = str_field(request, "name").unwrap_or("tool");
                let Some(summary) = summarise(name, request) else {
                    continue;
                };
                let part = Part::Tool(ToolCall {
                    id: str_field(request, "toolCallId").map(str::to_string),
                    name: name.to_string(),
                    summary,
                    failed: false,
                });
                conversation.push_part(Role::Assistant, at, part);
            }
        }
        "tool.execution_complete"
            if data.get("success").and_then(Value::as_bool) == Some(false) =>
        {
            if let Some(id) = str_field(data, "toolCallId") {
                conversation.mark_failed(id);
            }
        }
        _ => {}
    }
}

fn context_changed(conversation: &mut Conversation, context: &Value) {
    if conversation.project.is_none() {
        if let Some(cwd) = first_str(context, &["cwd", "gitRoot"]) {
            conversation.project = Some(cwd.into());
        }
    }
    if conversation.git_branch.is_none() {
        if let Some(branch) = first_str(context, &["branch"]) {
            conversation.git_branch = Some(branch.to_string());
        }
    }
}

/// A one-line summary of a tool request, or `None` for bookkeeping calls.
fn summarise(name: &str, request: &Value) -> Option<String> {
    let arguments = match request.get("arguments") {
        Some(Value::String(text)) => serde_json::from_str(text).unwrap_or(Value::Null),
        Some(other) => other.clone(),
        None => Value::Null,
    };
    let keys: &[&str] = match name {
        "report_intent" | "read_bash" | "write_bash" | "stop_bash" | "read_powershell"
        | "write_powershell" | "stop_powershell" => return None,
        "update_todo" | "todo_write" => return Some("updated the task list".to_string()),
        "bash" | "shell" | "powershell" => &["command"],
        "view" | "create" | "edit" | "str_replace_editor" => &["path", "file_path"],
        "grep" | "glob" | "rg" => &["pattern"],
        "task" => &["description"],
        "web_fetch" => &["url"],
        _ => &[
            "description",
            "command",
            "path",
            "file_path",
            "url",
            "query",
            "pattern",
            "name",
        ],
    };
    let summary = first_str(&arguments, keys)
        .or_else(|| first_str(request, &["intentionSummary", "toolTitle"]))
        .unwrap_or_default();
    Some(one_line(summary, SUMMARY_MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Agent;
    use serde_json::json;

    #[test]
    fn reads_turns_tools_and_failures() {
        let mut conversation = Conversation::new(Agent::CopilotCli, "dir-name");
        let events = [
            json!({"type": "session.start", "timestamp": "2026-01-02T10:00:00Z", "data": {"sessionId": "s-1", "selectedModel": "model-a", "context": {"cwd": "/work/app", "branch": "main"}}}),
            json!({"type": "user.message", "timestamp": "2026-01-02T10:00:01Z", "data": {"content": "List files", "transformedContent": "<context/> List files"}}),
            json!({"type": "user.message", "data": {"content": "injected", "source": "skill-pdf"}}),
            json!({"type": "assistant.message", "data": {"content": "", "toolRequests": [{"toolCallId": "c1", "name": "bash", "arguments": {"command": "ls"}}, {"toolCallId": "c0", "name": "report_intent", "arguments": {"intent": "x"}}]}}),
            json!({"type": "tool.execution_complete", "data": {"toolCallId": "c1", "success": false}}),
            json!({"type": "assistant.message", "data": {"content": "sub", "parentToolCallId": "c9"}}),
            json!({"type": "assistant.message", "data": {"content": "Done."}}),
            json!({"type": "session.title_changed", "data": {"title": "Listing files"}}),
        ];
        for event in &events {
            apply(&mut conversation, event);
        }
        assert_eq!(conversation.session_id, "s-1");
        assert_eq!(conversation.project.as_deref(), Some("/work/app".as_ref()));
        assert_eq!(conversation.git_branch.as_deref(), Some("main"));
        assert_eq!(conversation.title(), "Listing files");
        assert_eq!(conversation.messages.len(), 2);
        assert_eq!(conversation.messages[0].text(), "List files");
        let calls: Vec<_> = conversation.messages[1].tool_calls().collect();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].failed);
        assert_eq!(conversation.messages[1].text(), "Done.");
    }
}
