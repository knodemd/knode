//! Telegram Desktop exports: Settings > Advanced > Export Telegram data (the whole account) or
//! a chat's menu > Export chat history, in the machine-readable JSON format. See
//! <https://core.telegram.org/import-export>.
//!
//! The export is a folder with `result.json` and the media beside it (`photos/`, `files/`,
//! `voice_messages/`, `video_files/`...), referred to by paths relative to that folder. A chat
//! export is one chat: `{ "name", "type", "id", "messages" }`. An account export has
//! `chats.list` (and `left_chats.list`) with one such chat each.
//!
//! Each message has an `id`, a `type` (`message` or `service`), a `date` in the time of the
//! computer that exported it, `from`, `text` and `text_entities` (the text cut into plain and
//! formatted parts), `reply_to_message_id`, and its media: `photo`, or `file` with a
//! `media_type` (`voice_message`, `video_file`, `sticker`...). Media not downloaded are named
//! "(File not included. Change data exporting settings to download.)".

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use super::ChatFormat;
use super::files::{Files, file_name, join, parent};
use super::model::{Chat, ChatMessage, Media};
use super::render::escape_text;

/// How many bytes of `result.json` are read to tell it is a Telegram export.
const PEEK_BYTES: u64 = 4 * 1024;

/// The `result.json` of a Telegram export, if the files are one: the shallowest.
pub(crate) fn locate(files: &mut Files) -> Option<String> {
    let candidate = match &files.single {
        Some(single) if single.eq_ignore_ascii_case("result.json") => Some(single.clone()),
        Some(_) => None,
        None => files
            .names()
            .iter()
            .filter(|n| file_name(n) == "result.json")
            .min_by_key(|n| (n.matches('/').count(), n.len()))
            .cloned(),
    }?;
    let head = files.peek(&candidate, PEEK_BYTES).ok()?;
    let head = String::from_utf8_lossy(&head);
    let telegram = head.trim_start().starts_with('{')
        && (head.contains("\"messages\"")
            || head.contains("\"chats\"")
            || head.contains("\"personal_information\"")
            || head.contains("core.telegram.org"));
    telegram.then_some(candidate)
}

#[derive(Debug, Deserialize)]
struct Export {
    #[serde(default)]
    chats: Option<ChatList>,
    #[serde(default)]
    left_chats: Option<ChatList>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    messages: Option<Vec<Message>>,
}

#[derive(Debug, Default, Deserialize)]
struct ChatList {
    #[serde(default)]
    list: Vec<SingleChat>,
}

#[derive(Debug, Default, Deserialize)]
struct SingleChat {
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    messages: Option<Vec<Message>>,
}

#[derive(Debug, Deserialize)]
struct Message {
    id: i64,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    actor: Option<String>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    members: Vec<Option<String>>,
    #[serde(default)]
    text: Value,
    #[serde(default)]
    text_entities: Option<Vec<Entity>>,
    #[serde(default)]
    reply_to_message_id: Option<i64>,
    #[serde(default)]
    forwarded_from: Option<String>,
    #[serde(default)]
    photo: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    file_name: Option<String>,
    #[serde(default)]
    media_type: Option<String>,
    #[serde(default)]
    sticker_emoji: Option<String>,
    #[serde(default)]
    location_information: Option<Location>,
    #[serde(default)]
    poll: Option<Poll>,
}

#[derive(Debug, Deserialize)]
struct Entity {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    href: Option<String>,
    #[serde(default)]
    language: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Location {
    latitude: f64,
    longitude: f64,
}

#[derive(Debug, Deserialize)]
struct Poll {
    #[serde(default)]
    question: String,
    #[serde(default)]
    answers: Vec<PollAnswer>,
}

#[derive(Debug, Deserialize)]
struct PollAnswer {
    #[serde(default)]
    text: String,
    #[serde(default)]
    voters: u64,
}

/// Reads the chats of the export whose `result.json` is `file`.
pub(crate) fn read(files: &mut Files, file: &str) -> Result<Vec<Chat>, super::ChatImportError> {
    let reader = files.open_file(file)?;
    let export: Export = serde_json::from_reader(reader).map_err(|error| match error.is_io() {
        true => super::ChatImportError::Io(error.into()),
        false => super::ChatImportError::Json {
            file: file.to_string(),
            error,
        },
    })?;
    let dir = parent(file).to_string();
    let mut chats = Vec::new();
    let lists = [export.chats, export.left_chats];
    let mut all: Vec<SingleChat> = lists
        .into_iter()
        .flatten()
        .flat_map(|list| list.list)
        .collect();
    if export.messages.is_some() {
        all.push(SingleChat {
            name: export.name,
            kind: export.kind,
            id: export.id,
            messages: export.messages,
        });
    }
    for single in all {
        if let Some(chat) = chat(single, &dir, files) {
            chats.push(chat);
        }
    }
    Ok(chats)
}

fn chat(single: SingleChat, dir: &str, files: &Files) -> Option<Chat> {
    let id = match single.id? {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s,
        _ => return None,
    };
    let kind = single.kind.unwrap_or_default();
    let name = single
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| match kind.as_str() {
            "saved_messages" => "Saved Messages".to_string(),
            _ => format!("chat {id}"),
        });
    let name = name
        .replace(['[', ']', '|'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut chat = Chat::new(ChatFormat::Telegram, id, format!("Telegram - {name}"));
    chat.kind = match kind.as_str() {
        "personal_chat" | "bot_chat" | "saved_messages" => "personal",
        "private_group" | "private_supergroup" | "public_supergroup" => "group",
        "private_channel" | "public_channel" => "channel",
        _ => "chat",
    }
    .to_string();

    let mut messages = single.messages.unwrap_or_default();
    messages.sort_by_key(|m| m.id);
    // What a reply refers to: who wrote it, when and how it starts.
    let mut replied: BTreeMap<i64, (String, chrono::NaiveDateTime, String)> = BTreeMap::new();
    for m in &messages {
        if let Some(time) = parse_time(&m.date) {
            let who = m
                .from
                .clone()
                .or_else(|| m.actor.clone())
                .unwrap_or_default();
            let plain = crate::text::one_line(&plain_text(m), 40);
            replied.insert(m.id, (who, time, plain));
        }
    }
    for m in messages {
        let Some(time) = parse_time(&m.date) else {
            continue;
        };
        let mark = format!("{:020}", m.id);
        let mut message = if m.kind == "service" {
            ChatMessage::new(
                m.id.to_string(),
                mark,
                time,
                None,
                escape_text(&service_text(&m)),
            )
        } else {
            let sender = m
                .from
                .clone()
                .filter(|f| !f.trim().is_empty())
                .unwrap_or_else(|| name.clone());
            let mut text = markdown(&m);
            if let Some(from) = &m.forwarded_from {
                text = format!(
                    "*Forwarded from {}:* {text}",
                    super::render::escape_name(from)
                );
            }
            if let Some(location) = &m.location_information {
                text.push_str(&format!(
                    "\nLocation: {}, {}",
                    location.latitude, location.longitude
                ));
            }
            if let Some(poll) = &m.poll {
                text.push_str(&format!("\nPoll: {}", escape_text(&poll.question)));
                for answer in &poll.answers {
                    text.push_str(&format!(
                        "\n- {} ({})",
                        escape_text(&answer.text),
                        answer.voters
                    ));
                }
            }
            let mut message = ChatMessage::new(m.id.to_string(), mark, time, Some(sender), text);
            message.media = media(&m, dir, files);
            message
        };
        if let Some(reply) = m.reply_to_message_id {
            message.reply_to = Some(match replied.get(&reply) {
                Some((who, at, plain)) => {
                    let when = if at.date() == time.date() {
                        at.format("%H:%M").to_string()
                    } else {
                        at.format("%Y-%m-%d %H:%M").to_string()
                    };
                    let who = super::render::escape_name(if who.is_empty() { &name } else { who });
                    if plain.is_empty() {
                        format!("{who} at {when}")
                    } else {
                        format!("{who} at {when}: \"{}\"", escape_text(plain))
                    }
                }
                None => format!("message {reply}"),
            });
        }
        chat.messages.push(message);
    }
    chat.collect_participants();
    Some(chat)
}

fn parse_time(date: &str) -> Option<chrono::NaiveDateTime> {
    chrono::NaiveDateTime::parse_from_str(date, "%Y-%m-%dT%H:%M:%S").ok()
}

/// What the export says instead of a path for a file it did not download.
fn not_included(path: &str) -> bool {
    path.starts_with('(') && path.ends_with(')')
}

fn media(m: &Message, dir: &str, files: &Files) -> Vec<Media> {
    let mut media = Vec::new();
    let what = match m.media_type.as_deref() {
        Some("voice_message") => "voice message",
        Some("video_message") => "video message",
        Some("video_file") => "video",
        Some("audio_file") => "audio",
        Some("animation") => "animation",
        Some("sticker") => "sticker",
        _ => "file",
    };
    let mut one = |path: &str, fallback: &str| {
        if not_included(path) {
            media.push(Media::Omitted {
                what: fallback.to_string(),
            });
            return;
        }
        let entry = join(dir, path);
        let name = file_name(path).to_string();
        if files.contains(&entry) {
            media.push(Media::File { name, entry });
        } else {
            media.push(Media::Missing { name });
        }
    };
    if let Some(photo) = &m.photo {
        one(photo, "photo");
    }
    if let Some(file) = &m.file {
        // A sticker is shown by its emoji; its file is left out.
        if m.media_type.as_deref() != Some("sticker") {
            one(file, m.file_name.as_deref().unwrap_or(what));
        }
    }
    media
}

fn service_text(m: &Message) -> String {
    let actor = m.actor.clone().unwrap_or_default();
    let members: Vec<&str> = m.members.iter().flatten().map(String::as_str).collect();
    let title = m.title.as_deref().unwrap_or("");
    let action = m.action.as_deref().unwrap_or("");
    let did = match action {
        "create_group" | "create_channel" => format!("created \"{title}\""),
        "invite_members" => format!("added {}", members.join(", ")),
        "remove_members" => format!("removed {}", members.join(", ")),
        "join_group_by_link" => "joined by link".to_string(),
        "edit_group_title" => format!("changed the title to \"{title}\""),
        "edit_group_photo" => "changed the photo".to_string(),
        "delete_group_photo" => "removed the photo".to_string(),
        "pin_message" => "pinned a message".to_string(),
        "phone_call" => "called".to_string(),
        "migrate_to_supergroup" | "migrate_from_group" => "upgraded the group".to_string(),
        "" => plain_text(m),
        other => other.replace('_', " "),
    };
    if actor.is_empty() {
        did
    } else {
        format!("{actor} {did}")
    }
}

/// The text's parts: from `text_entities`, else from `text` (a string, or strings and
/// `{ type, text, href }` objects).
fn entities(m: &Message) -> Vec<Entity> {
    if let Some(entities) = &m.text_entities {
        return entities
            .iter()
            .map(|e| Entity {
                kind: e.kind.clone(),
                text: e.text.clone(),
                href: e.href.clone(),
                language: e.language.clone(),
            })
            .collect();
    }
    let plain = |text: &str| Entity {
        kind: "plain".into(),
        text: text.to_string(),
        href: None,
        language: None,
    };
    match &m.text {
        Value::String(s) => vec![plain(s)],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part {
                Value::String(s) => Some(plain(s)),
                Value::Object(_) => serde_json::from_value(part.clone()).ok(),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn plain_text(m: &Message) -> String {
    entities(m).into_iter().map(|e| e.text).collect()
}

/// The text of a message as Markdown: bold, italic, struck out, code, links and quotes kept.
fn markdown(m: &Message) -> String {
    let mut out = String::new();
    for e in entities(m) {
        out.push_str(&entity_markdown(&e));
    }
    if out.trim().is_empty()
        && let Some(emoji) = &m.sticker_emoji
    {
        return format!("{emoji} (sticker)");
    }
    out
}

fn entity_markdown(e: &Entity) -> String {
    let text = e.text.as_str();
    let wrap = |marker: &str| {
        let core = text.trim();
        if core.is_empty() {
            return escape_text(text);
        }
        let start = &text[..text.len() - text.trim_start().len()];
        let end = &text[text.trim_end().len()..];
        format!("{start}{marker}{}{marker}{end}", escape_text(core))
    };
    match e.kind.as_str() {
        "bold" => wrap("**"),
        "italic" => wrap("*"),
        "strikethrough" => wrap("~~"),
        "code" => {
            let fence = if text.contains('`') { "``" } else { "`" };
            let pad = if text.contains('`') { " " } else { "" };
            format!("{fence}{pad}{text}{pad}{fence}")
        }
        "pre" => {
            let language = e.language.as_deref().unwrap_or("");
            format!("\n```{language}\n{}\n```\n", text.trim_end_matches('\n'))
        }
        "text_link" => match &e.href {
            Some(href) => format!(
                "[{}](<{}>)",
                escape_text(text).replace(['[', ']'], ""),
                href.replace(['<', '>'], "")
            ),
            None => escape_text(text),
        },
        "link" if text.starts_with("http://") || text.starts_with("https://") => {
            format!("<{}>", text.replace(['<', '>'], ""))
        }
        "blockquote" => text
            .lines()
            .map(|line| format!("> {}", escape_text(line)))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => escape_text(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(kind: &str, text: &str) -> Entity {
        Entity {
            kind: kind.into(),
            text: text.into(),
            href: None,
            language: None,
        }
    }

    #[test]
    fn turns_entities_into_markdown() {
        assert_eq!(entity_markdown(&entity("bold", " big ")), " **big** ");
        assert_eq!(entity_markdown(&entity("italic", "")), "");
        assert_eq!(entity_markdown(&entity("code", "a`b")), "`` a`b ``");
        assert_eq!(entity_markdown(&entity("plain", "[[x]]")), "\\[\\[x\\]\\]");
        assert_eq!(
            entity_markdown(&entity("blockquote", "one\ntwo")),
            "> one\n> two"
        );
        assert_eq!(
            entity_markdown(&entity("link", "example.com")),
            "example.com"
        );
        let mut link = entity("text_link", "the [plan]");
        link.href = Some("https://example.com/<p>".into());
        assert_eq!(
            entity_markdown(&link),
            "[the plan](<https://example.com/p>)"
        );
    }

    #[test]
    fn describes_what_telegram_wrote() {
        let message: Message = serde_json::from_value(serde_json::json!({
            "id": 1, "type": "service", "date": "2026-03-04T09:00:00",
            "actor": "Ada", "action": "invite_members", "members": ["Grace", null, "Linus"]
        }))
        .unwrap();
        assert_eq!(service_text(&message), "Ada added Grace, Linus");
    }
}
