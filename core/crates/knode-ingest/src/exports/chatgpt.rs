//! The ChatGPT data export.
//!
//! Requested from Settings, Data controls, Export data; it arrives by email as a `.zip` with
//! `conversations.json` and `chat.html` at its top, beside `user.json`,
//! `message_feedback.json` and the files of the conversations. OpenAI documents how to request
//! it (help.openai.com, "How do I export my ChatGPT history and data?") but not its structure:
//! the fields read here are the ones the export has been seen to carry and that open-source
//! export viewers read. Unknown fields are ignored, so a change of the export degrades instead
//! of failing.
//!
//! `conversations.json` is an array of conversations. Each holds its messages as a tree, since
//! editing a message or regenerating an answer starts a new branch:
//!
//! ```json
//! {"id": "…", "conversation_id": "…", "title": "Title",
//!  "create_time": 1735812000.123, "update_time": 1735812300.5,
//!  "current_node": "n3", "default_model_slug": "gpt-4o",
//!  "mapping": {
//!    "n1": {"id": "n1", "parent": null, "children": ["n2"], "message": null},
//!    "n2": {"id": "n2", "parent": "n1", "children": ["n3"], "message": {
//!      "author": {"role": "user" | "assistant" | "tool" | "system", "name": null},
//!      "create_time": 1735812001.0, "recipient": "all",
//!      "content": {"content_type": "text", "parts": ["…"]},
//!      "metadata": {"model_slug": "gpt-4o"}}}}}
//! ```
//!
//! Only the branch that ends at `current_node` (the one the person last saw) is read, from the
//! root down. System messages, hidden context (custom instructions, memory) and reasoning are
//! left out. An assistant message sent to a tool (a `recipient` other than `all`, such as
//! `python`, `browser`, `web` or `dalle.text2im`) becomes a tool call of one line; tool output
//! is left out, except images it made. Images (`image_asset_pointer` parts) become
//! placeholders.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::model::{Agent, Conversation, Part, Role, Title, TitleSource, ToolCall};
use crate::text::{SUMMARY_MAX, first_str, one_line, str_field};

/// One conversation of `conversations.json`.
pub(crate) fn conversation(value: &Value) -> Option<Conversation> {
    let id = first_str(value, &["conversation_id", "id"])?;
    let mut conversation = Conversation::new(Agent::ChatGpt, id);
    if let Some(title) = str_field(value, "title").filter(|t| !t.trim().is_empty()) {
        conversation.recorded_title = Some(Title {
            text: title.to_string(),
            source: TitleSource::Agent,
        });
    }
    for key in ["create_time", "update_time"] {
        if let Some(at) = seconds(value.get(key)) {
            conversation.saw_time(at);
        }
    }
    let mapping = value.get("mapping").and_then(Value::as_object);
    if let Some(mapping) = mapping {
        for (node_id, message) in branch(mapping, str_field(value, "current_node")) {
            add_message(&mut conversation, node_id, message);
        }
    }
    if conversation.model.is_none() {
        conversation.model = str_field(value, "default_model_slug")
            .filter(|slug| *slug != "auto")
            .map(str::to_string);
    }
    Some(conversation)
}

/// The messages of the branch that ends at `current`, from the root down, with the id of their
/// node. Without a known `current` node, the branch that always takes the last child.
fn branch<'a>(
    mapping: &'a serde_json::Map<String, Value>,
    current: Option<&str>,
) -> Vec<(&'a str, &'a Value)> {
    let mut nodes: Vec<&'a str> = Vec::new();
    match current.and_then(|id| mapping.get_key_value(id)) {
        Some((id, _)) => {
            let mut next = Some(id.as_str());
            while let Some(id) = next {
                // A cycle would be a broken export: stop instead of looping.
                if nodes.len() > mapping.len() || nodes.contains(&id) {
                    break;
                }
                nodes.push(id);
                next = mapping
                    .get(id)
                    .and_then(|node| str_field(node, "parent"))
                    .and_then(|parent| mapping.get_key_value(parent))
                    .map(|(id, _)| id.as_str());
            }
            nodes.reverse();
        }
        None => {
            let root = mapping.iter().find(|(_, node)| {
                node.get("parent")
                    .and_then(Value::as_str)
                    .is_none_or(|parent| !mapping.contains_key(parent))
            });
            let mut next = root.map(|(id, _)| id.as_str());
            while let Some(id) = next {
                if nodes.len() > mapping.len() || nodes.contains(&id) {
                    break;
                }
                nodes.push(id);
                next = mapping
                    .get(id)
                    .and_then(|node| node.get("children")?.as_array()?.last()?.as_str())
                    .and_then(|child| mapping.get_key_value(child))
                    .map(|(id, _)| id.as_str());
            }
        }
    }
    nodes
        .into_iter()
        .filter_map(|id| {
            let message = mapping.get(id)?.get("message")?;
            message.is_object().then_some((id, message))
        })
        .collect()
}

fn add_message(conversation: &mut Conversation, node_id: &str, message: &Value) {
    let metadata = message.get("metadata").unwrap_or(&Value::Null);
    let hidden = [
        "is_visually_hidden_from_conversation",
        "is_user_system_message",
    ]
    .iter()
    .any(|key| metadata.get(key).and_then(Value::as_bool) == Some(true));
    if hidden {
        return;
    }
    let at = seconds(message.get("create_time"));
    let content = message.get("content").unwrap_or(&Value::Null);
    let content_type = str_field(content, "content_type").unwrap_or("text");
    let role = message
        .get("author")
        .and_then(|author| str_field(author, "role"));
    match role {
        Some("user") => {
            if matches!(content_type, "text" | "multimodal_text") {
                note_time(conversation, at);
                add_parts(conversation, Role::User, at, content);
            }
        }
        Some("assistant") => {
            note_time(conversation, at);
            if let Some(slug) = str_field(metadata, "model_slug") {
                conversation.model = Some(slug.to_string());
            }
            let recipient = str_field(message, "recipient").unwrap_or("all");
            if recipient != "all" {
                let call = ToolCall {
                    id: Some(node_id.to_string()),
                    name: recipient.to_string(),
                    summary: tool_summary(content),
                    failed: false,
                };
                conversation.push_part(Role::Assistant, at, Part::Tool(call));
                return;
            }
            match content_type {
                "text" | "multimodal_text" => add_parts(conversation, Role::Assistant, at, content),
                "code" => {
                    if let Some(code) = str_field(content, "text").filter(|t| !t.trim().is_empty())
                    {
                        let language = str_field(content, "language")
                            .filter(|l| *l != "unknown")
                            .unwrap_or("");
                        let text = format!("```{language}\n{}\n```", code.trim_end());
                        conversation.push_part(Role::Assistant, at, Part::Text { text });
                    }
                }
                // Reasoning, and content types not known yet, are left out.
                _ => {}
            }
        }
        Some("tool") => {
            let failed = content_type == "system_error"
                || metadata
                    .get("aggregate_result")
                    .and_then(|result| str_field(result, "status"))
                    .is_some_and(|status| status.starts_with("failed"));
            if failed {
                // The result follows the call it answers.
                if let Some(id) = last_call_id(conversation) {
                    conversation.mark_failed(&id);
                }
            }
            // Images a tool made, such as a generated picture, belong to the answer.
            if content_type == "multimodal_text" {
                let images = content
                    .get("parts")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|part| is_image(part))
                    .count();
                for _ in 0..images {
                    conversation.push_part(Role::Assistant, at, Part::Image);
                }
            }
        }
        _ => {}
    }
}

fn note_time(conversation: &mut Conversation, at: Option<DateTime<Utc>>) {
    if let Some(at) = at {
        conversation.saw_time(at);
    }
}

/// The id of the last tool call, which a failed tool result belongs to.
fn last_call_id(conversation: &Conversation) -> Option<String> {
    conversation
        .messages
        .last()?
        .tool_calls()
        .last()?
        .id
        .clone()
}

/// The text and images of a `text` or `multimodal_text` content.
fn add_parts(
    conversation: &mut Conversation,
    role: Role,
    at: Option<DateTime<Utc>>,
    content: &Value,
) {
    let parts = content.get("parts").and_then(Value::as_array);
    for part in parts.into_iter().flatten() {
        if let Some(text) = part.as_str() {
            let text = clean(text);
            if !text.trim().is_empty() {
                conversation.push_part(role, at, Part::Text { text });
            }
        } else if is_image(part) {
            conversation.push_part(role, at, Part::Image);
        } else if str_field(part, "content_type") == Some("audio_transcription") {
            if let Some(text) = str_field(part, "text").filter(|t| !t.trim().is_empty()) {
                conversation.push_part(role, at, Part::Text { text: text.into() });
            }
        }
    }
}

fn is_image(part: &Value) -> bool {
    str_field(part, "content_type") == Some("image_asset_pointer")
}

/// A one-line summary of what an assistant message sent to a tool: the query, prompt or URL of
/// a JSON request, else the first line of the code or text.
fn tool_summary(content: &Value) -> String {
    let text = match str_field(content, "text") {
        Some(text) => text.to_string(),
        None => content
            .get("parts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n"),
    };
    if let Ok(request) = serde_json::from_str::<Value>(&text) {
        let keys = ["query", "q", "prompt", "url", "path", "title", "name"];
        let found = first_str(&request, &keys).map(str::to_string).or_else(|| {
            let first = request
                .get("search_query")
                .or_else(|| request.get("open"))?
                .as_array()?
                .first()?
                .clone();
            first_str(&first, &keys).map(str::to_string)
        });
        if let Some(found) = found {
            return one_line(&found, SUMMARY_MAX);
        }
    }
    one_line(&text, SUMMARY_MAX)
}

/// Text without the citation markers ChatGPT writes into answers: `【4†source】` and the
/// private-use characters around `cite` references.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let next = rest.find(['\u{e200}', '【']);
        let Some(start) = next else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let (close, len) = if after.starts_with('【') {
            ('】', '】'.len_utf8())
        } else {
            ('\u{e201}', '\u{e201}'.len_utf8())
        };
        match after.find(close) {
            Some(end) if after.starts_with('\u{e200}') || after[..end].contains('†') => {
                rest = &after[end + len..];
            }
            _ => {
                let first = after.chars().next().map_or(1, char::len_utf8);
                out.push_str(&after[..first]);
                rest = &after[first..];
            }
        }
    }
    out.chars()
        .filter(|c| !('\u{e000}'..='\u{f8ff}').contains(c))
        .collect()
}

/// A time given in seconds since the Unix epoch, with a fraction.
fn seconds(value: Option<&Value>) -> Option<DateTime<Utc>> {
    let seconds = value?.as_f64()?;
    DateTime::from_timestamp_millis((seconds * 1000.0).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, parent: Option<&str>, children: &[&str], message: Value) -> Value {
        json!({"id": id, "parent": parent, "children": children, "message": message})
    }

    fn text(role: &str, text: &str, at: f64) -> Value {
        json!({"author": {"role": role}, "create_time": at, "recipient": "all",
               "content": {"content_type": "text", "parts": [text]}, "metadata": {}})
    }

    #[test]
    fn follows_the_current_branch() {
        let value = json!({
            "conversation_id": "conv-1", "title": "Branches", "current_node": "b2",
            "create_time": 1735812000.0, "update_time": 1735812100.0,
            "mapping": {
                "root": node("root", None, &["sys"], Value::Null),
                "sys": node("sys", Some("root"), &["u1"], text("system", "You are…", 0.0)),
                "u1": node("u1", Some("sys"), &["a1", "b1"], text("user", "Question", 1735812001.0)),
                "a1": node("a1", Some("u1"), &[], text("assistant", "Old answer", 1735812002.0)),
                "b1": node("b1", Some("u1"), &["b2"], text("assistant", "New answer", 1735812003.0)),
                "b2": node("b2", Some("b1"), &[], text("user", "Thanks", 1735812004.0))
            }
        });
        let conversation = conversation(&value).unwrap();
        let texts: Vec<String> = conversation.messages.iter().map(|m| m.text()).collect();
        assert_eq!(texts, ["Question", "New answer", "Thanks"]);
        assert_eq!(conversation.title(), "Branches");
        assert_eq!(conversation.session_id, "conv-1");
    }

    #[test]
    fn falls_back_to_the_last_child_without_a_current_node() {
        let value = json!({"id": "conv-2", "mapping": {
            "u1": node("u1", None, &["a1", "b1"], text("user", "Q", 1.0)),
            "a1": node("a1", Some("u1"), &[], text("assistant", "A", 2.0)),
            "b1": node("b1", Some("u1"), &[], text("assistant", "B", 3.0))
        }});
        let conversation = conversation(&value).unwrap();
        assert_eq!(conversation.messages[1].text(), "B");
    }

    #[test]
    fn condenses_tools_and_keeps_images() {
        let value = json!({"id": "conv-3", "current_node": "a2", "mapping": {
            "u1": node("u1", None, &["c1"], json!({"author": {"role": "user"},
                "content": {"content_type": "multimodal_text", "parts": [
                    {"content_type": "image_asset_pointer", "asset_pointer": "file-service://x"},
                    "What is in it?"]}})),
            "c1": node("c1", Some("u1"), &["o1"], json!({"author": {"role": "assistant"},
                "recipient": "python", "metadata": {"model_slug": "gpt-4o"},
                "content": {"content_type": "code", "language": "python",
                            "text": "import os\nprint(1)"}})),
            "o1": node("o1", Some("c1"), &["w1"], json!({"author": {"role": "tool", "name": "python"},
                "content": {"content_type": "execution_output", "text": "Traceback"},
                "metadata": {"aggregate_result": {"status": "failed_with_in_kernel_exception"}}})),
            "w1": node("w1", Some("o1"), &["d1"], json!({"author": {"role": "assistant"},
                "recipient": "web.run", "content": {"content_type": "text",
                "parts": ["{\"search_query\": [{\"q\": \"weather Lyon\"}]}"]}})),
            "d1": node("d1", Some("w1"), &["a2"], json!({"author": {"role": "tool", "name": "dalle.text2im"},
                "content": {"content_type": "multimodal_text", "parts": [
                    {"content_type": "image_asset_pointer", "asset_pointer": "sediment://y"}]}})),
            "a2": node("a2", Some("d1"), &[], json!({"author": {"role": "assistant"},
                "recipient": "all", "content": {"content_type": "text",
                "parts": ["Sunny \u{e200}cite\u{e202}turn0search0\u{e201} today【3†source】."]}}))
        }});
        let conversation = conversation(&value).unwrap();
        assert_eq!(conversation.model.as_deref(), Some("gpt-4o"));
        let [user, assistant] = conversation.messages.as_slice() else {
            panic!("two turns expected");
        };
        assert_eq!(user.parts[0], Part::Image);
        assert_eq!(user.text(), "What is in it?");
        let calls: Vec<_> = assistant.tool_calls().collect();
        assert_eq!(calls[0].name, "python");
        assert_eq!(calls[0].summary, "import os…");
        assert!(calls[0].failed);
        assert_eq!(calls[1].name, "web.run");
        assert_eq!(calls[1].summary, "weather Lyon");
        assert!(assistant.parts.contains(&Part::Image));
        assert_eq!(assistant.text(), "Sunny  today.");
    }

    #[test]
    fn leaves_out_hidden_context() {
        let value = json!({"id": "conv-4", "current_node": "u1", "mapping": {
            "ctx": node("ctx", None, &["u1"], json!({"author": {"role": "user"},
                "content": {"content_type": "user_editable_context", "user_profile": "x"},
                "metadata": {"is_visually_hidden_from_conversation": true}})),
            "u1": node("u1", Some("ctx"), &[], text("user", "Hi", 5.0))
        }});
        let conversation = conversation(&value).unwrap();
        assert_eq!(conversation.messages.len(), 1);
        assert_eq!(conversation.started_at.unwrap().timestamp(), 5);
    }
}
