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

use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

use serde::Deserialize;
use serde::de::{
    DeserializeSeed, Deserializer, Error as _, IgnoredAny, MapAccess, SeqAccess, Visitor,
};
use serde_json::Value;

use super::files::{Files, file_name, join, parent};
use super::model::{Chat, ChatMessage, Media};
use super::render::escape_text;
use super::write::ChatWriter;
use super::{ChatFormat, ChatImportError};

/// How many bytes of `result.json` are read to tell it is a Telegram export.
const PEEK_BYTES: u64 = 4 * 1024;

/// The `result.json` of a Telegram export, if the files are one: in `root` (see
/// [`super::in_root`]), the shallowest.
pub(crate) fn locate(files: &mut Files, root: Option<&str>) -> Option<String> {
    let candidate = match &files.single {
        Some(single) if single.eq_ignore_ascii_case("result.json") => Some(single.clone()),
        Some(_) => None,
        None => files
            .names()
            .iter()
            .filter(|n| super::in_root(n, root) && file_name(n) == "result.json")
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

/// The most bytes of `result.json` read for one chat. An account export is read one chat at a
/// time, each written before the next is read, so it can be far larger than this; an export
/// of one chat is one chat.
pub(crate) const MAX_CHAT_SIZE: u64 = super::files::MAX_FILE_SIZE;

/// Imports the chats of the export whose `result.json` is `file`, read from `source` (the
/// export opened a second time), with their media from `files`: each chat is written as soon
/// as it is read, so only one is in memory at a time.
pub(crate) fn import(
    source: &mut Files,
    file: &str,
    files: &mut Files,
    writer: &mut ChatWriter<'_>,
) -> Result<(), ChatImportError> {
    import_with_limit(source, file, files, writer, MAX_CHAT_SIZE)
}

fn import_with_limit(
    source: &mut Files,
    file: &str,
    files: &mut Files,
    writer: &mut ChatWriter<'_>,
    limit: u64,
) -> Result<(), ChatImportError> {
    let dir = parent(file).to_string();
    let mut redacted = 0;
    let mut write = |mut single: SingleChat| -> Result<(), ChatImportError> {
        for m in single.messages.iter_mut().flatten() {
            redacted += redact(m);
        }
        if let Some(chat) = chat(single, &dir, files) {
            writer.write(&chat, files)?;
        }
        Ok(())
    };
    let read = Rc::new(Cell::new(0));
    let reader = std::io::BufReader::new(source.open_file_counted(file, limit, read.clone())?);
    let mut failed = None;
    let mut single = None;
    let mut json = serde_json::Deserializer::from_reader(reader);
    let parsed = ExportSeed {
        each: &mut write,
        failed: &mut failed,
        single: &mut single,
        read: &read,
    }
    .deserialize(&mut json)
    .and_then(|()| json.end());
    if let Some(error) = failed {
        return Err(error);
    }
    parsed.map_err(|error| match error.is_io() {
        true => ChatImportError::Io(error.into()),
        false => ChatImportError::Json {
            file: file.to_string(),
            error,
        },
    })?;
    if let Some(single) = single {
        write(single)?;
    }
    super::note_redacted(&mut writer.report, redacted);
    Ok(())
}

/// Reads `result.json`: the chats of `chats.list` and `left_chats.list` are handed to `each`
/// one by one as they are read, and the chat of an export of one chat is kept in `single`.
struct ExportSeed<'a, F> {
    each: &'a mut F,
    failed: &'a mut Option<ChatImportError>,
    single: &'a mut Option<SingleChat>,
    read: &'a Rc<Cell<u64>>,
}

impl<'de, F> DeserializeSeed<'de> for ExportSeed<'_, F>
where
    F: FnMut(SingleChat) -> Result<(), ChatImportError>,
{
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de, F> Visitor<'de> for ExportSeed<'_, F>
where
    F: FnMut(SingleChat) -> Result<(), ChatImportError>,
{
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a Telegram Desktop export")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut chat = SingleChat::default();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "chats" | "left_chats" => map.next_value_seed(ListSeed {
                    each: &mut *self.each,
                    failed: &mut *self.failed,
                    read: self.read,
                })?,
                "name" => chat.name = map.next_value()?,
                "type" => chat.kind = map.next_value()?,
                "id" => chat.id = map.next_value()?,
                "messages" => chat.messages = map.next_value()?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        if chat.messages.is_some() {
            *self.single = Some(chat);
        }
        Ok(())
    }
}

/// `{ "list": [...] }` of chats, each handed to `each` as soon as it is read.
struct ListSeed<'a, F> {
    each: &'a mut F,
    failed: &'a mut Option<ChatImportError>,
    read: &'a Rc<Cell<u64>>,
}

impl<'de, F> DeserializeSeed<'de> for ListSeed<'_, F>
where
    F: FnMut(SingleChat) -> Result<(), ChatImportError>,
{
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de, F> Visitor<'de> for ListSeed<'_, F>
where
    F: FnMut(SingleChat) -> Result<(), ChatImportError>,
{
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a list of chats")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            if key == "list" {
                map.next_value_seed(ListSeed {
                    each: &mut *self.each,
                    failed: &mut *self.failed,
                    read: self.read,
                })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        // The limit is on each chat: the count starts again after every one.
        self.read.set(0);
        while let Some(chat) = seq.next_element::<SingleChat>()? {
            if let Err(error) = (self.each)(chat) {
                *self.failed = Some(error);
                return Err(A::Error::custom("the import stopped"));
            }
            self.read.set(0);
        }
        Ok(())
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }
}

/// Replaces the secrets in what a message says (its text, the parts of it, their links and
/// a poll), and says how many there were: the text and its parts say the same, so the more of
/// the two counts.
fn redact(m: &mut Message) -> usize {
    fn one(text: &mut String) -> usize {
        let (out, n) = crate::redact::redact_secrets(text);
        if n > 0 {
            *text = out;
        }
        n
    }
    fn value(v: &mut Value) -> usize {
        match v {
            Value::String(s) => one(s),
            Value::Array(parts) => parts.iter_mut().map(value).sum(),
            Value::Object(fields) => fields
                .iter_mut()
                .filter(|(k, _)| matches!(k.as_str(), "text" | "href"))
                .map(|(_, v)| value(v))
                .sum(),
            _ => 0,
        }
    }
    let in_text = value(&mut m.text);
    let mut in_parts = 0;
    for e in m.text_entities.iter_mut().flatten() {
        in_parts += one(&mut e.text);
        in_parts += e.href.as_mut().map_or(0, one);
    }
    let mut n = in_text.max(in_parts);
    if let Some(poll) = &mut m.poll {
        n += one(&mut poll.question);
        for answer in &mut poll.answers {
            n += one(&mut answer.text);
        }
    }
    n
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
            // A span ends at the first run of as many backticks as opened it: one more than
            // the longest run in the code cannot be in it. Markdown takes one space off each
            // end, so the code keeps its own when it starts or ends with a backtick or a space.
            let fence = "`".repeat(longest_backticks(text) + 1);
            let pad = if text.contains('`')
                || (text.starts_with(' ') && text.ends_with(' ') && !text.trim().is_empty())
            {
                " "
            } else {
                ""
            };
            format!("{fence}{pad}{text}{pad}{fence}")
        }
        "pre" => {
            // A fence longer than any run of backticks in the code, so the code cannot end it.
            let fence = "`".repeat((longest_backticks(text) + 1).max(3));
            let language = e.language.as_deref().unwrap_or("").replace(['`', '\n'], "");
            format!(
                "\n{fence}{language}\n{}\n{fence}\n",
                text.trim_end_matches('\n')
            )
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

/// The longest run of backticks in `text`.
fn longest_backticks(text: &str) -> usize {
    text.split(|c| c != '`').map(str::len).max().unwrap_or(0)
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
        assert_eq!(entity_markdown(&entity("code", "a``b")), "``` a``b ```");
        assert_eq!(entity_markdown(&entity("code", "`x")), "`` `x ``");
        assert_eq!(entity_markdown(&entity("code", " x ")), "`  x  `");
        assert_eq!(entity_markdown(&entity("code", "plain")), "`plain`");
        assert_eq!(entity_markdown(&entity("plain", "[[x]]")), "\\[\\[x\\]\\]");
        assert_eq!(
            entity_markdown(&entity("blockquote", "one\ntwo")),
            "> one\n> two"
        );
        assert_eq!(
            entity_markdown(&entity("link", "example.com")),
            "example.com"
        );
        let mut pre = entity("pre", "a\n```\n## 2026-03-20");
        pre.language = Some("md".into());
        assert_eq!(
            entity_markdown(&pre),
            "\n````md\na\n```\n## 2026-03-20\n````\n"
        );
        assert_eq!(
            entity_markdown(&entity("plain", "x\n## 2026-03-20")),
            "x\n\\## 2026-03-20"
        );
        let mut link = entity("text_link", "the [plan]");
        link.href = Some("https://example.com/<p>".into());
        assert_eq!(
            entity_markdown(&link),
            "[the plan](<https://example.com/p>)"
        );
    }

    fn account_export(chats: usize, words: usize) -> String {
        let list: Vec<String> = (1..=chats)
            .map(|id| {
                format!(
                    r#"{{"name":"Chat {id}","type":"personal_chat","id":{id},"messages":[{{"id":1,"type":"message","date":"2026-03-04T09:00:00","from":"Ada","text":"{}"}}]}}"#,
                    "word ".repeat(words)
                )
            })
            .collect();
        format!(
            r#"{{"about":"x","personal_information":{{"first_name":"Ada"}},"chats":{{"about":"y","list":[{}]}},"left_chats":{{"list":[]}}}}"#,
            list.join(",")
        )
    }

    fn import_limited(dir: &std::path::Path, limit: u64) -> Result<usize, ChatImportError> {
        let mut space = knode_format::Space::create(dir.join("T.knode"), "T").unwrap();
        let mut writer = ChatWriter {
            space: &mut space,
            by: knode_format::Provenance::person("example"),
            report: super::super::ChatImportReport::new(ChatFormat::Telegram),
        };
        let path = dir.join("export");
        let mut source = Files::open(&path).unwrap();
        let mut files = Files::open(&path).unwrap();
        import_with_limit(&mut source, "result.json", &mut files, &mut writer, limit)?;
        Ok(writer.report.chats.len())
    }

    #[test]
    fn reads_an_account_export_one_chat_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("export")).unwrap();
        // Ten chats of about 2 KB each: the whole file is over the limit, each chat under it.
        let json = account_export(10, 400);
        assert!(json.len() > 16 * 1024);
        std::fs::write(dir.path().join("export/result.json"), &json).unwrap();
        assert_eq!(import_limited(dir.path(), 16 * 1024).unwrap(), 10);

        // One chat over the limit stops the import and says why.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("export")).unwrap();
        std::fs::write(
            dir.path().join("export/result.json"),
            account_export(1, 8_000),
        )
        .unwrap();
        let error = import_limited(dir.path(), 16 * 1024).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("a chat in result.json is larger than"),
            "{error}"
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
