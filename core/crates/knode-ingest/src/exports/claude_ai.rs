//! The claude.ai data export.
//!
//! Requested from Settings, Privacy, Export data; it arrives as a `.zip` with
//! `conversations.json`, `projects.json` and `users.json` at its top. The structure is not
//! documented by Anthropic: the fields read here are the ones the export has been seen to carry
//! and that open-source export viewers read. Unknown fields are ignored and missing ones left
//! out, so a change of the export degrades instead of failing.
//!
//! `conversations.json` is an array of conversations:
//!
//! ```json
//! {"uuid": "…", "name": "Title", "created_at": "2025-01-02T10:00:00.000000Z",
//!  "updated_at": "…", "chat_messages": [
//!    {"uuid": "…", "sender": "human" | "assistant", "created_at": "…", "text": "…",
//!     "content": [{"type": "text", "text": "…"},
//!                 {"type": "thinking", "thinking": "…"},
//!                 {"type": "tool_use", "id": "…", "name": "web_search", "input": {…}},
//!                 {"type": "tool_result", "tool_use_id": "…", "is_error": false, …}],
//!     "attachments": [{"file_name": "notes.txt", "extracted_content": "…"}],
//!     "files": [{"file_name": "photo.png"}]}]}
//! ```
//!
//! Thinking and tool results are left out, as in capture; the text of attachments is left out
//! too, and each attachment or file becomes a placeholder with its name. A conversation's
//! `project_uuid` (or `project.uuid`), when present, links it to its project; that field is not
//! verified against a real export.
//!
//! `projects.json` is an array of projects:
//!
//! ```json
//! {"uuid": "…", "name": "Project", "description": "…", "prompt_template": "…",
//!  "created_at": "…", "docs": [{"filename": "brief.md", "content": "…"}]}
//! ```

use serde_json::Value;

use super::file_part;
use crate::model::{Agent, Conversation, Part, Role, Title, TitleSource, ToolCall};
use crate::render::demote_headings;
use crate::text::{SUMMARY_MAX, first_str, one_line, str_field, timestamp};

/// One conversation of `conversations.json`, and the uuid of its project if it names one.
pub(crate) fn conversation(value: &Value) -> Option<(Conversation, Option<String>)> {
    let uuid = str_field(value, "uuid").filter(|id| !id.trim().is_empty())?;
    let mut conversation = Conversation::new(Agent::ClaudeAi, uuid);
    if let Some(name) = str_field(value, "name").filter(|name| !name.trim().is_empty()) {
        conversation.recorded_title = Some(Title {
            text: name.to_string(),
            source: TitleSource::Agent,
        });
    }
    conversation.model = str_field(value, "model").map(str::to_string);
    for key in ["created_at", "updated_at"] {
        if let Some(at) = timestamp(value.get(key)) {
            conversation.saw_time(at);
        }
    }
    let messages = value.get("chat_messages").and_then(Value::as_array);
    for message in messages.into_iter().flatten() {
        add_message(&mut conversation, message);
    }
    let project = str_field(value, "project_uuid")
        .or_else(|| str_field(value.get("project")?, "uuid"))
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string);
    Some((conversation, project))
}

fn add_message(conversation: &mut Conversation, message: &Value) {
    let role = match str_field(message, "sender") {
        Some("human") => Role::User,
        Some("assistant") => Role::Assistant,
        _ => return,
    };
    let at = timestamp(message.get("created_at"));
    if let Some(at) = at {
        conversation.saw_time(at);
    }
    let blocks = message
        .get("content")
        .and_then(Value::as_array)
        .filter(|blocks| !blocks.is_empty());
    match blocks {
        Some(blocks) => {
            for block in blocks {
                add_block(conversation, role, at, block);
            }
        }
        None => {
            if let Some(text) = str_field(message, "text").filter(|t| !t.trim().is_empty()) {
                conversation.push_part(role, at, Part::Text { text: text.into() });
            }
        }
    }
    let mut names: Vec<&str> = Vec::new();
    for key in ["attachments", "files", "files_v2"] {
        let files = message.get(key).and_then(Value::as_array);
        for file in files.into_iter().flatten() {
            if let Some(name) = str_field(file, "file_name").filter(|n| !n.trim().is_empty()) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    for name in names {
        conversation.push_part(role, at, file_part(name));
    }
}

fn add_block(
    conversation: &mut Conversation,
    role: Role,
    at: Option<chrono::DateTime<chrono::Utc>>,
    block: &Value,
) {
    match str_field(block, "type") {
        Some("text") => {
            if let Some(text) = str_field(block, "text").filter(|t| !t.trim().is_empty()) {
                conversation.push_part(role, at, Part::Text { text: text.into() });
            }
        }
        Some("tool_use") => {
            let name = str_field(block, "name").unwrap_or("tool").to_string();
            let input = block.get("input").unwrap_or(&Value::Null);
            let keys = [
                "query",
                "url",
                "command",
                "title",
                "description",
                "path",
                "code",
                "name",
            ];
            let summary = first_str(input, &keys)
                .map(|text| one_line(text, SUMMARY_MAX))
                .unwrap_or_default();
            let call = ToolCall {
                id: str_field(block, "id").map(str::to_string),
                name,
                summary,
                failed: false,
            };
            conversation.push_part(role, at, Part::Tool(call));
        }
        Some("tool_result") => {
            if block.get("is_error").and_then(Value::as_bool) == Some(true) {
                if let Some(id) = str_field(block, "tool_use_id") {
                    conversation.mark_failed(id);
                }
            }
        }
        Some("image") => conversation.push_part(role, at, Part::Image),
        // Thinking, voice notes and blocks not known yet are left out.
        _ => {}
    }
}

/// A project of `projects.json`, as the node it becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Project {
    pub(crate) uuid: String,
    pub(crate) title: String,
    pub(crate) body: String,
}

/// One project of `projects.json`: its description, then its instructions and the documents of
/// its knowledge, each under a heading.
pub(crate) fn project(value: &Value) -> Option<Project> {
    let uuid = str_field(value, "uuid").filter(|id| !id.trim().is_empty())?;
    let title = str_field(value, "name")
        .map(|name| name.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Claude project".to_string());
    let mut body = String::new();
    if let Some(description) = first_str(value, &["description"]) {
        body.push_str(description.trim());
        body.push('\n');
    }
    if let Some(instructions) = first_str(value, &["prompt_template"]) {
        push_section(&mut body, "## Instructions");
        body.push_str(&demote_headings(instructions.trim()));
        body.push('\n');
    }
    let docs = value.get("docs").and_then(Value::as_array);
    let docs: Vec<&Value> = docs.into_iter().flatten().collect();
    if !docs.is_empty() {
        push_section(&mut body, "## Knowledge");
        for doc in docs {
            let name = str_field(doc, "filename").unwrap_or("Untitled");
            if !body.ends_with("\n\n") {
                body.push('\n');
            }
            body.push_str(&format!("### {}\n", name.trim()));
            if let Some(content) = first_str(doc, &["content"]) {
                body.push('\n');
                body.push_str(&demote_headings(content.trim()));
                body.push('\n');
            }
        }
    }
    Some(Project {
        uuid: uuid.to_string(),
        title: title.replace('[', "(").replace(']', ")").replace('|', "/"),
        body,
    })
}

fn push_section(body: &mut String, heading: &str) {
    if !body.is_empty() {
        body.push('\n');
    }
    body.push_str(heading);
    body.push_str("\n\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_text_tools_and_files() {
        let value = json!({
            "uuid": "c-1", "name": "Trip plan",
            "created_at": "2025-01-02T10:00:00.000000Z",
            "updated_at": "2025-01-02T10:05:00Z",
            "project_uuid": "p-1",
            "chat_messages": [
                {"sender": "human", "created_at": "2025-01-02T10:00:01Z", "text": "Plan it",
                 "content": [{"type": "text", "text": "Plan it"}],
                 "attachments": [{"file_name": "notes.txt", "extracted_content": "secret"}],
                 "files": [{"file_name": "map.PNG"}]},
                {"sender": "assistant", "created_at": "2025-01-02T10:00:05Z",
                 "content": [
                    {"type": "thinking", "thinking": "hmm"},
                    {"type": "tool_use", "id": "t1", "name": "web_search",
                     "input": {"query": "trains\nto Lyon"}},
                    {"type": "tool_result", "tool_use_id": "t1", "is_error": true},
                    {"type": "text", "text": "Here it is."}]},
                {"sender": "system", "text": "ignored"}
            ]
        });
        let (conversation, project) = conversation(&value).unwrap();
        assert_eq!(project.as_deref(), Some("p-1"));
        assert_eq!(conversation.title(), "Trip plan");
        assert_eq!(conversation.messages.len(), 2);
        let user = &conversation.messages[0];
        assert_eq!(user.text(), "Plan it\n\n_[attached: notes.txt]_");
        assert_eq!(user.parts[2], Part::Image);
        let assistant = &conversation.messages[1];
        let call = assistant.tool_calls().next().unwrap();
        assert_eq!(call.summary, "trains…");
        assert!(call.failed);
        assert_eq!(assistant.text(), "Here it is.");
        assert_eq!(
            conversation.ended_at.unwrap().to_rfc3339(),
            "2025-01-02T10:05:00+00:00"
        );
    }

    #[test]
    fn falls_back_to_the_text_field() {
        let value = json!({"uuid": "c-2", "chat_messages": [
            {"sender": "human", "text": "Old style", "content": []}]});
        let (conversation, project) = conversation(&value).unwrap();
        assert!(project.is_none());
        assert_eq!(conversation.messages[0].text(), "Old style");
        assert!(super::conversation(&json!({"name": "no id"})).is_none());
    }

    #[test]
    fn writes_projects_with_instructions_and_knowledge() {
        let value = json!({"uuid": "p-1", "name": "Garden [2025]",
            "description": "Planting notes", "prompt_template": "# Be brief",
            "docs": [{"filename": "soil.md", "content": "# Soil\nLoam"}]});
        let project = project(&value).unwrap();
        assert_eq!(project.title, "Garden (2025)");
        assert_eq!(
            project.body,
            "Planting notes\n\n## Instructions\n\n### Be brief\n\n## Knowledge\n\n\
             ### soil.md\n\n### Soil\nLoam\n"
        );
    }
}
