//! iMessage, read live from the Messages database of this Mac: [`IMessageConnector`].
//!
//! Messages keeps every conversation in `~/Library/Messages/chat.db`, a SQLite database, and
//! the files sent in them in `~/Library/Messages/Attachments`. Reading them needs Full Disk
//! Access. The database is never opened where it is: it is copied, with its write-ahead log,
//! into a private temporary folder and only the copy is read
//! ([`sqlite::open_copy`](crate::sources::sqlite::open_copy)). Before anything is read, the
//! copy is checked for every table and column Knode reads; when one is missing, nothing is
//! written and the error says the connector needs an update.
//!
//! Every conversation becomes one node of kind [`CHAT_KIND`](super::CHAT_KIND), the same as a
//! chat export: its messages grouped by day under `## YYYY-MM-DD`, each `**Sender** (HH:MM):
//! text`, the files sent attached when the Mac has them, and the properties of
//! [`keys`](super::keys) with `chat.app` `imessage` and `chat.id` the conversation's guid.
//!
//! - The text is the `text` column, or, when it is empty (as it often is since macOS 13), the
//!   text archived in `attributedBody`, read by a small typedstream reader.
//! - Times are Apple times (nanoseconds since 2001-01-01) shown in the Mac's time zone.
//! - People are their phone numbers and email addresses; a [`ContactNames`] turns them into
//!   names (the Mac app, which may ask for Contacts access, can provide one; the core never
//!   reads Contacts).
//! - Reactions (tapbacks) are left out: they are not messages, and a reaction taken back would
//!   otherwise stay in the node. Edited messages show their last text, marked edited; messages
//!   taken back show that one was. A message edited or taken back after it was brought in keeps
//!   the text it had then: messages already in a node are never read again.
//! - The cursor is the number (`ROWID`) of the last message read. Conversations excluded by
//!   guid are never read. With [`History::FromToday`], nothing sent before the day the
//!   connector was connected is read. The cursor also names that message's guid: when Messages
//!   numbered its messages again (a rebuilt database, or one moved from another Mac), every
//!   message is read once more and each node takes only those after the last one it has, found
//!   by its guid.

mod db;
mod typedstream;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, FixedOffset, Local, NaiveDateTime, Utc};
use knode_format::{ChangeOp, Provenance, Space, Timestamp};

use super::files::Files;
use super::model::{Chat, ChatMessage, Media};
use super::render::{escape_name, escape_text};
use super::write::ChatWriter;
use super::{ChatFormat, ChatImportError, ChatImportReport, keys};
use crate::sources::{
    Batch, Connector, Container, Cursor, FetchRequest, Fetched, History, Permission, SourceError,
    SourceInfo, Trigger, Written,
};
use db::{ChatRow, MessageRow, Snapshot, failed};

/// The id of the connector, and of the app in `chat.app`.
pub const APP: &str = "imessage";

/// Seconds from 1970-01-01 to 2001-01-01, where Apple times start.
const APPLE_EPOCH: i64 = 978_307_200;

/// `chat.style` of a group conversation.
const STYLE_GROUP: i64 = 43;

/// The longest time between two runs when no change of the database was seen.
const RUN_AT_LEAST_EVERY: Duration = Duration::from_secs(15 * 60);

/// The Messages folder of the person running Knode: `~/Library/Messages`.
pub fn default_messages_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Messages"))
}

/// Turns phone numbers and email addresses into the names of people, such as from Contacts.
pub trait ContactNames: Send + Sync {
    /// The name of whoever has this phone number or email address, if known.
    fn name(&self, handle: &str) -> Option<String>;
}

/// How messages are shown.
#[derive(Clone)]
struct Options {
    me: String,
    names: Option<Arc<dyn ContactNames>>,
    offset: Option<FixedOffset>,
}

/// The iMessage connector: reads the conversations of Messages on this Mac, from a copy of its
/// database, into one `chat` node per conversation.
#[derive(Clone)]
pub struct IMessageConnector {
    /// The Messages folder, with `chat.db` and `Attachments` in it.
    dir: PathBuf,
    options: Options,
}

impl fmt::Debug for IMessageConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IMessageConnector")
            .field("dir", &self.dir)
            .field("me", &self.options.me)
            .field("names", &self.options.names.is_some())
            .field("offset", &self.options.offset)
            .finish()
    }
}

impl IMessageConnector {
    /// The connector for the Messages folder of the person running Knode.
    pub fn local() -> Option<IMessageConnector> {
        default_messages_dir().map(IMessageConnector::new)
    }

    /// The connector for a Messages folder.
    pub fn new(dir: impl Into<PathBuf>) -> IMessageConnector {
        IMessageConnector {
            dir: dir.into(),
            options: Options {
                me: "Me".to_string(),
                names: None,
                offset: None,
            },
        }
    }

    /// The same connector, naming the person `me` as a sender (`Me` by default).
    pub fn with_me(mut self, me: impl Into<String>) -> Self {
        self.options.me = me.into();
        self
    }

    /// The same connector, naming people with `names` instead of their phone numbers and
    /// email addresses.
    pub fn with_names(mut self, names: Arc<dyn ContactNames>) -> Self {
        self.options.names = Some(names);
        self
    }

    /// The same connector, showing times in `offset` instead of the Mac's time zone.
    pub fn with_offset(mut self, offset: FixedOffset) -> Self {
        self.options.offset = Some(offset);
        self
    }

    /// The database.
    pub fn database(&self) -> PathBuf {
        self.dir.join("chat.db")
    }

    fn names(&self) -> Option<&dyn ContactNames> {
        self.options.names.as_deref()
    }
}

impl Connector for IMessageConnector {
    fn info(&self) -> SourceInfo {
        SourceInfo::connector(
            APP,
            "Messages",
            "The iMessage and SMS conversations of Messages on this Mac",
        )
        .needing([Permission::FullDiskAccess])
        .excluding("conversation")
    }

    fn trigger(&self) -> Trigger {
        let db = self.database();
        Trigger::Watch {
            paths: vec![db.clone(), db::wal_path(&db)],
            every: RUN_AT_LEAST_EVERY,
        }
    }

    fn check(&self) -> Result<(), SourceError> {
        Snapshot::take(&self.database()).map(|_| ())
    }

    fn containers(&self, _request: &FetchRequest<'_>) -> Result<Vec<Container>, SourceError> {
        let snapshot = Snapshot::take(&self.database())?;
        Ok(snapshot
            .chats()
            .map_err(failed)?
            .iter()
            .map(|chat| {
                let participants = people(chat, self.names());
                Container::new(chat.guid.clone(), name(chat, &participants))
            })
            .collect())
    }

    fn fetch(&self, request: &FetchRequest<'_>) -> Result<Fetched, SourceError> {
        let snapshot = Snapshot::take(&self.database())?;
        let (mut after, seen) = parse_cursor(request.cursor);
        let reset = renumbered(&snapshot, after, seen.as_deref())?;
        if reset {
            after = 0;
        }
        let from = match request.history {
            History::Everything => None,
            History::FromToday => Some(apple_time(request.since)),
        };
        let chats: BTreeMap<i64, ChatRow> = snapshot
            .chats()
            .map_err(failed)?
            .into_iter()
            .filter(|c| !request.is_excluded(&c.guid))
            .map(|c| (c.rowid, c))
            .collect();
        let ids: Vec<i64> = chats.keys().copied().collect();
        // After a renumbering everything is read in one batch, so every conversation's node is
        // matched against all of its messages at once.
        let limit = if reset {
            usize::MAX - 1
        } else {
            request.max_items.max(1)
        };
        // One more than asked for says whether there is more.
        let mut rows = snapshot
            .messages(&ids, after, from, limit + 1)
            .map_err(failed)?;
        let more = rows.len() > limit;
        if more {
            // Never cut a message in two conversations at once.
            let last = rows[limit - 1].1.rowid;
            rows.retain(|(_, m)| m.rowid <= last);
        }
        let newest = snapshot.last_rowid().map_err(failed)?;
        let stop = if more {
            rows.last().map_or(newest, |(_, m)| m.rowid)
        } else {
            newest.max(after)
        };
        let cursor = Cursor::new(match snapshot.guid_at(stop).map_err(failed)? {
            Some(guid) => format!("{stop} {guid}"),
            None => stop.to_string(),
        });

        let mut by_chat: BTreeMap<i64, Chat> = BTreeMap::new();
        let mut items = 0;
        for (chat_id, message) in rows {
            let Some(row) = chats.get(&chat_id) else {
                continue;
            };
            // Reactions (tapbacks) and reactions taken back.
            if (2000..4000).contains(&message.associated_type) {
                continue;
            }
            let Some(m) = to_message(&snapshot, &message, row, &self.options, &self.dir)? else {
                continue;
            };
            let chat = by_chat.entry(chat_id).or_insert_with(|| {
                let participants = people(row, self.names());
                let mut chat = Chat::new(
                    ChatFormat::IMessage,
                    row.guid.clone(),
                    title(row, &participants),
                );
                chat.kind = kind(row).to_string();
                chat.participants = participants;
                chat.participants.push(self.options.me.clone());
                chat
            });
            chat.messages.push(m);
            items += 1;
        }
        if by_chat.is_empty() && !reset {
            return Ok(Fetched::nothing(cursor).with_more(more));
        }
        let batch = IMessageBatch {
            chats: by_chat.into_values().collect(),
            dir: self.dir.clone(),
            items,
            reset,
        };
        Ok(Fetched::batch(batch, cursor).with_more(more))
    }
}

/// What a cursor holds: the number of the last message read (0 for none) and, when it was
/// recorded, that message's guid: `<rowid> <guid>`.
fn parse_cursor(cursor: Option<&Cursor>) -> (i64, Option<String>) {
    let Some(text) = cursor.map(|c| c.as_str().trim()) else {
        return (0, None);
    };
    let (number, guid) = match text.split_once(' ') {
        Some((number, guid)) => (number, Some(guid.trim().to_string())),
        None => (text, None),
    };
    match number.parse() {
        Ok(rowid) => (rowid, guid.filter(|g| !g.is_empty())),
        Err(_) => (0, None),
    }
}

/// Whether Messages numbered its messages again since the cursor was recorded (its database
/// was rebuilt, or comes from another Mac): there are fewer messages than the cursor says, or
/// the message the cursor names is now at another number.
fn renumbered(snapshot: &Snapshot, after: i64, seen: Option<&str>) -> Result<bool, SourceError> {
    if after == 0 {
        return Ok(false);
    }
    if snapshot.last_rowid().map_err(failed)? < after {
        return Ok(true);
    }
    let Some(seen) = seen else {
        return Ok(false);
    };
    if let Some(guid) = snapshot.guid_at(after).map_err(failed)? {
        return Ok(guid != seen);
    }
    // No message at that number: deleted, or moved.
    Ok(snapshot
        .rowid_of(seen)
        .map_err(failed)?
        .is_some_and(|rowid| rowid != after))
}

/// A time in milliseconds since 1970 as an Apple time.
fn apple_time(at: Timestamp) -> i64 {
    let secs = at.0.div_euclid(1000) - APPLE_EPOCH;
    let millis = at.0.rem_euclid(1000);
    secs.saturating_mul(1_000_000_000)
        .saturating_add(millis * 1_000_000)
}

/// Messages read from Messages, written with the chat importers' writer.
struct IMessageBatch {
    chats: Vec<Chat>,
    /// The Messages folder, where the files sent are.
    dir: PathBuf,
    items: usize,
    /// Messages numbered its messages again: these are all the messages it has, and the
    /// nodes written before are matched against them by message guid.
    reset: bool,
}

impl Batch for IMessageBatch {
    fn len(&self) -> usize {
        // A batch after a renumbering is written even with no new message, to move the nodes'
        // positions to the new numbers.
        self.items.max(usize::from(self.reset))
    }

    fn write(self: Box<Self>, space: &mut Space, by: &Provenance) -> Result<Written, SourceError> {
        let mut files = Files::unlisted(&self.dir);
        let mut writer = ChatWriter {
            space,
            by: by.clone(),
            report: ChatImportReport::new(ChatFormat::IMessage),
        };
        let to_source = |e: ChatImportError| match e {
            ChatImportError::Space(e) => SourceError::Space(e),
            other => SourceError::Failed(other.to_string()),
        };
        if self.reset {
            rebase_untouched(writer.space, &self.chats, by)?;
        }
        for chat in &self.chats {
            if self.reset {
                let unseen = rebase(&mut writer, chat).map_err(to_source)?;
                writer.write(&unseen, &mut files).map_err(to_source)?;
            } else {
                writer.write(chat, &mut files).map_err(to_source)?;
            }
        }
        let report = writer.report;
        Ok(Written {
            items: report.messages,
            created: report
                .chats
                .iter()
                .filter(|c| c.op == ChangeOp::Create)
                .count(),
            updated: report
                .chats
                .iter()
                .filter(|c| c.op == ChangeOp::Update)
                .count(),
        })
    }
}

/// The position no message is before.
fn first_mark() -> String {
    format!("{:020}", 0)
}

/// After Messages numbered its messages again, the nodes of conversations that have no
/// message now start again from the first number, so what comes later is added.
fn rebase_untouched(space: &mut Space, chats: &[Chat], by: &Provenance) -> Result<(), SourceError> {
    for summary in space.find_by_property(keys::APP, APP)? {
        let id = space.property(&summary.id, keys::ID)?.unwrap_or_default();
        if chats.iter().any(|c| c.id == id) {
            continue;
        }
        space.set_property(&summary.id, keys::LAST, &first_mark(), by)?;
        space.set_property(&summary.id, keys::LAST_KEYS, "", by)?;
    }
    Ok(())
}

/// After Messages numbered its messages again, the messages of `chat` the node does not have
/// yet: those after the last one it has, found by its guid (or, when Messages no longer has
/// it, by its time). The node's position is moved to the new number of that message, so the
/// next runs go on from it.
fn rebase(writer: &mut ChatWriter<'_>, chat: &Chat) -> Result<Chat, ChatImportError> {
    let mut unseen = chat.clone();
    let Some(id) = writer.find(chat)? else {
        return Ok(unseen);
    };
    let space = &mut *writer.space;
    let last_keys: Vec<String> = space
        .property(&id, keys::LAST_KEYS)?
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let ended = space.property(&id, keys::ENDED_AT)?.unwrap_or_default();
    let time = |m: &ChatMessage| m.time.format("%Y-%m-%dT%H:%M:%S").to_string();
    let had = chat
        .messages
        .iter()
        .rposition(|m| last_keys.contains(&m.key))
        .or_else(|| {
            chat.messages
                .iter()
                .rposition(|m| !ended.is_empty() && time(m) <= ended)
        });
    let (mark, keys_at) = match had {
        Some(i) => (chat.messages[i].mark.clone(), chat.messages[i].key.clone()),
        None => (first_mark(), String::new()),
    };
    space.set_property(&id, keys::LAST, &mark, &writer.by)?;
    space.set_property(&id, keys::LAST_KEYS, &keys_at, &writer.by)?;
    unseen.messages = match had {
        Some(i) => chat.messages[i + 1..].to_vec(),
        None => chat.messages.clone(),
    };
    Ok(unseen)
}

/// An Apple time as a time of day in `offset` (the Mac's when `None`).
fn local_time(date: i64, offset: Option<FixedOffset>) -> NaiveDateTime {
    // Before macOS 10.13 times were in seconds; since then, in nanoseconds.
    let (secs, nanos) = if date.abs() >= 1_000_000_000_000 {
        (
            date.div_euclid(1_000_000_000),
            date.rem_euclid(1_000_000_000),
        )
    } else {
        (date, 0)
    };
    let utc = DateTime::<Utc>::from_timestamp(secs.saturating_add(APPLE_EPOCH), nanos as u32)
        .unwrap_or_default();
    match offset {
        Some(offset) => utc.with_timezone(&offset).naive_local(),
        None => utc.with_timezone(&Local).naive_local(),
    }
}

fn kind(chat: &ChatRow) -> &'static str {
    if chat.style == STYLE_GROUP || chat.handles.len() > 1 {
        "group"
    } else {
        "personal"
    }
}

/// A handle as a name.
fn person(handle: &str, names: Option<&dyn ContactNames>) -> String {
    names
        .and_then(|n| n.name(handle))
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| handle.to_string())
}

/// The others in a conversation.
fn people(chat: &ChatRow, names: Option<&dyn ContactNames>) -> Vec<String> {
    let mut people: Vec<String> = Vec::new();
    for handle in &chat.handles {
        let name = person(handle, names);
        if !people.contains(&name) {
            people.push(name);
        }
    }
    if people.is_empty() && !chat.identifier.is_empty() {
        people.push(person(&chat.identifier, names));
    }
    people
}

fn title(chat: &ChatRow, participants: &[String]) -> String {
    format!("iMessage - {}", name(chat, participants))
}

/// The name of a conversation: the one it was given, or who is in it.
fn name(chat: &ChatRow, participants: &[String]) -> String {
    let name = if !chat.display_name.trim().is_empty() {
        chat.display_name.trim().to_string()
    } else if !participants.is_empty() {
        crate::text::one_line(&participants.join(", "), 80)
    } else {
        "Conversation".to_string()
    };
    let name: String = name
        .chars()
        .map(|c| if matches!(c, '[' | ']' | '|') { ' ' } else { c })
        .collect();
    name.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The text of a message: its `text`, or the text archived in `attributedBody`, without the
/// placeholders Messages puts where files go.
fn text_of(message: &MessageRow) -> String {
    let text = message
        .text
        .clone()
        .filter(|t| !t.is_empty())
        .or_else(|| {
            message
                .body
                .as_deref()
                .and_then(typedstream::attributed_string_text)
        })
        .unwrap_or_default();
    text.replace(['\u{FFFC}', '\u{FFFD}'], "")
        .trim()
        .to_string()
}

fn sender(message: &MessageRow, chat: &ChatRow, options: &Options) -> String {
    if message.from_me {
        return options.me.clone();
    }
    match &message.handle {
        Some(handle) if !handle.is_empty() => person(handle, options.names.as_deref()),
        _ => match chat.handles.as_slice() {
            [only] => person(only, options.names.as_deref()),
            _ => "Unknown".to_string(),
        },
    }
}

fn to_message(
    snapshot: &Snapshot,
    message: &MessageRow,
    chat: &ChatRow,
    options: &Options,
    dir: &Path,
) -> Result<Option<ChatMessage>, SourceError> {
    let time = local_time(message.date, options.offset);
    let mark = format!("{:020}", message.rowid);
    let who = sender(message, chat, options);
    if message.item_type != 0 {
        // Group events: only a new name for the group is kept.
        let Some(name) = message
            .group_title
            .as_deref()
            .filter(|_| message.item_type == 2)
            .filter(|n| !n.trim().is_empty())
        else {
            return Ok(None);
        };
        let text = format!(
            "{} named the conversation \u{201c}{}\u{201d}",
            escape_name(&who),
            escape_name(name.trim())
        );
        return Ok(Some(ChatMessage::new(
            message.guid.clone(),
            mark,
            time,
            None,
            text,
        )));
    }

    let text = text_of(message);
    let media: Vec<Media> = message
        .attachments
        .iter()
        .map(|a| attachment(a, dir))
        .collect();
    let unsent = message.date_retracted != 0
        || (message.date_edited != 0 && text.is_empty() && media.is_empty());
    if text.is_empty() && media.is_empty() && !unsent {
        return Ok(None);
    }
    let mut m = ChatMessage::new(
        message.guid.clone(),
        mark,
        time,
        Some(who),
        escape_text(&text),
    );
    m.media = media;
    if unsent {
        m.notes.push("*[unsent]*".to_string());
    } else if message.date_edited != 0 {
        m.notes.push("*(edited)*".to_string());
    }
    if let Some(guid) = message.reply_to.as_deref().filter(|g| !g.is_empty())
        && guid != message.guid
        && let Some(original) = snapshot.message_by_guid(guid).map_err(failed)?
    {
        let who = sender(&original, chat, options);
        let at = local_time(original.date, options.offset).format("%H:%M");
        let said = crate::text::one_line(&text_of(&original), 40);
        m.reply_to = Some(if said.is_empty() {
            format!("{} at {at}", escape_name(&who))
        } else {
            format!("{} at {at}: {}", escape_name(&who), escape_text(&said))
        });
    }
    Ok(Some(m))
}

/// A file sent with a message: attached when it is in the Messages folder on this Mac.
fn attachment(row: &db::AttachmentRow, dir: &Path) -> Media {
    let path = row.filename.as_deref().unwrap_or_default();
    let name = row
        .transfer_name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .or_else(|| {
            Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| {
            match row.mime_type.as_deref().and_then(|m| m.split('/').next()) {
                Some("image") => "photo",
                Some("video") => "video",
                Some("audio") => "audio",
                _ => "file",
            }
            .to_string()
        });
    let not_attached = |reason: &str| Media::NotAttached {
        name: name.clone(),
        reason: reason.to_string(),
    };
    let Some(entry) = entry_in(path, dir) else {
        return not_attached("not on this Mac");
    };
    let mut full = dir.to_path_buf();
    full.extend(entry.split('/'));
    if !full.is_file() {
        return not_attached("not on this Mac");
    }
    Media::File { name, entry }
}

/// Where a file Messages names is inside the Messages folder, with `/` between folders, or
/// `None` when it is outside it.
fn entry_in(path: &str, dir: &Path) -> Option<String> {
    let rest = if let Some(rest) = path.strip_prefix("~/Library/Messages/") {
        PathBuf::from(rest)
    } else {
        Path::new(path).strip_prefix(dir).ok()?.to_path_buf()
    };
    let mut parts = Vec::new();
    for component in rest.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_string()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if parts.first().map(String::as_str) != Some("Attachments") || parts.len() < 2 {
        return None;
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_apple_times() {
        let utc = FixedOffset::east_opt(0);
        // 2026-03-04 09:15:00 UTC.
        let secs = 794_308_500;
        assert_eq!(
            local_time(secs * 1_000_000_000, utc).to_string(),
            "2026-03-04 09:15:00"
        );
        assert_eq!(local_time(secs, utc).to_string(), "2026-03-04 09:15:00");
        let madrid = FixedOffset::east_opt(3600);
        assert_eq!(
            local_time(secs * 1_000_000_000, madrid).to_string(),
            "2026-03-04 10:15:00"
        );
    }

    #[test]
    fn reads_the_start_of_the_history_as_an_apple_time() {
        let utc = FixedOffset::east_opt(0);
        let since = Timestamp((APPLE_EPOCH + 794_308_500) * 1000 + 250);
        let apple = apple_time(since);
        assert_eq!(apple, 794_308_500_250_000_000);
        assert_eq!(
            local_time(apple, utc).to_string(),
            "2026-03-04 09:15:00.250"
        );
    }

    #[test]
    fn cursors_are_message_numbers() {
        assert_eq!(parse_cursor(None), (0, None));
        assert_eq!(parse_cursor(Some(&Cursor::new("42"))), (42, None));
        assert_eq!(
            parse_cursor(Some(&Cursor::new("42 A-B-C"))),
            (42, Some("A-B-C".to_string()))
        );
        assert_eq!(parse_cursor(Some(&Cursor::new("not a number"))), (0, None));
    }

    #[test]
    fn finds_files_only_in_the_attachments_folder() {
        let dir = Path::new("/Users/example/Library/Messages");
        assert_eq!(
            entry_in("~/Library/Messages/Attachments/ab/01/X/IMG_1.heic", dir).as_deref(),
            Some("Attachments/ab/01/X/IMG_1.heic")
        );
        assert_eq!(
            entry_in(
                "/Users/example/Library/Messages/Attachments/ab/IMG_1.heic",
                dir
            )
            .as_deref(),
            Some("Attachments/ab/IMG_1.heic")
        );
        assert_eq!(entry_in("~/Library/Messages/chat.db", dir), None);
        assert_eq!(
            entry_in("~/Library/Messages/Attachments/../chat.db", dir),
            None
        );
        assert_eq!(entry_in("/private/var/tmp/sticker.png", dir), None);
        assert_eq!(entry_in("", dir), None);
    }

    #[test]
    fn titles_name_the_conversation() {
        let mut chat = ChatRow {
            rowid: 1,
            guid: "iMessage;-;+15550100".into(),
            style: 45,
            identifier: "+15550100".into(),
            display_name: String::new(),
            handles: vec!["+15550100".into()],
        };
        assert_eq!(title(&chat, &people(&chat, None)), "iMessage - +15550100");
        chat.display_name = "Trip [2026]".into();
        assert_eq!(title(&chat, &[]), "iMessage - Trip 2026");
    }
}
