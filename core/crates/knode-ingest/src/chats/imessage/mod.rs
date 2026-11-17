//! iMessage, read live from the Messages database of this Mac: [`sync`].
//!
//! Messages keeps every conversation in `~/Library/Messages/chat.db`, a SQLite database, and
//! the files sent in them in `~/Library/Messages/Attachments`. Reading them needs Full Disk
//! Access. The database is never opened where it is: it is copied, with its write-ahead log,
//! into a private temporary folder, the copy is backed up into a snapshot, and only the
//! snapshot is read ([`db`]). Before anything is read, the snapshot is checked for every table
//! and column Knode reads; when one is missing, nothing is written and the error says the
//! connector needs an update.
//!
//! Every conversation becomes one node of kind [`CHAT_KIND`](super::CHAT_KIND), the same as a
//! chat export: its messages grouped by day under `## YYYY-MM-DD`, each `**Sender** (HH:MM):
//! text`, the files sent attached when the Mac has them, and the properties of
//! [`keys`](super::keys) with `chat.app` `imessage` and `chat.id` the conversation's guid.
//!
//! - The text is the `text` column, or, when it is empty (as it often is since macOS 13), the
//!   text archived in `attributedBody`, read by a small typedstream reader.
//! - Times are Apple times (nanoseconds since 2001-01-01) shown in the Mac's time zone.
//! - People are their phone numbers and email addresses; a [`ContactNames`] given in the
//!   options turns them into names (the Mac app, which may ask for Contacts access, provides
//!   one; the core never reads Contacts).
//! - Reactions (tapbacks) are left out: they are not messages, and a reaction taken back would
//!   otherwise stay in the node. Edited messages show their last text, marked edited; messages
//!   taken back show that one was.
//! - Where to go on from is a [`Cursor`], the number of the last message read. Conversations
//!   excluded by guid are never read. On the first sync, [`History`] says whether to bring
//!   everything Messages has or only what is sent from today on.

mod db;
mod typedstream;

use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, FixedOffset, Local, NaiveDateTime, NaiveTime, TimeZone, Utc};
use knode_format::{Provenance, Space};

pub use db::Stamp;

use super::files::Files;
use super::model::{Chat, ChatMessage, Media};
use super::render::{escape_name, escape_text};
use super::write::ChatWriter;
use super::{ChatFormat, ChatImportError, ChatImportReport, keys};
use db::{ChatRow, MessageRow, Snapshot};

/// The id of the app in `chat.app`.
pub const APP: &str = "imessage";

/// Seconds from 1970-01-01 to 2001-01-01, where Apple times start.
const APPLE_EPOCH: i64 = 978_307_200;

/// `chat.style` of a group conversation.
const STYLE_GROUP: i64 = 43;

/// The Messages folder of the person running Knode: `~/Library/Messages`.
pub fn default_messages_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Messages"))
}

/// Where Messages keeps its database and files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagesSource {
    /// The Messages folder, with `chat.db` and `Attachments` in it.
    pub dir: PathBuf,
}

impl MessagesSource {
    /// The Messages folder of the person running Knode.
    pub fn local() -> Option<MessagesSource> {
        default_messages_dir().map(MessagesSource::new)
    }

    /// A Messages folder.
    pub fn new(dir: impl Into<PathBuf>) -> MessagesSource {
        MessagesSource { dir: dir.into() }
    }

    /// The database.
    pub fn database(&self) -> PathBuf {
        self.dir.join("chat.db")
    }

    /// The size and time of change of the database and its log: when it is the same as at
    /// the last sync, nothing new has arrived and there is no need to copy the database.
    pub fn stamp(&self) -> Result<Stamp, IMessageError> {
        db::stamp(&self.database())
    }
}

/// What to bring on the first sync of a space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum History {
    /// Only messages sent from the start of today on.
    #[default]
    FromToday,
    /// Every message Messages has.
    Everything,
}

/// Where a sync stopped: the number (`ROWID`) of the last message in the database then. The
/// next sync reads only messages after it. The caller keeps it for each space (as text, with
/// `to_string` and `parse`) and gives it back to the next sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cursor(pub i64);

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for Cursor {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.trim().parse().map(Cursor)
    }
}

/// Turns phone numbers and email addresses into the names of people, such as from Contacts.
pub trait ContactNames {
    /// The name of whoever has this phone number or email address, if known.
    fn name(&self, handle: &str) -> Option<String>;
}

/// How to sync.
pub struct SyncOptions<'a> {
    /// What to bring when the space has no cursor yet.
    pub history: History,
    /// The guids of the conversations never to read.
    pub excluded: BTreeSet<String>,
    /// How the person is named as a sender: `Me` by default.
    pub me: String,
    /// Names for phone numbers and email addresses; without it, they are shown as they are.
    pub names: Option<&'a dyn ContactNames>,
    /// The time zone times are shown in; the Mac's when `None`.
    pub offset: Option<FixedOffset>,
    /// The time now, which says when today starts; the clock's when `None`.
    pub now: Option<DateTime<Utc>>,
}

impl Default for SyncOptions<'_> {
    fn default() -> Self {
        SyncOptions {
            history: History::default(),
            excluded: BTreeSet::new(),
            me: "Me".to_string(),
            names: None,
            offset: None,
            now: None,
        }
    }
}

impl fmt::Debug for SyncOptions<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyncOptions")
            .field("history", &self.history)
            .field("excluded", &self.excluded)
            .field("me", &self.me)
            .field("names", &self.names.is_some())
            .field("offset", &self.offset)
            .field("now", &self.now)
            .finish()
    }
}

/// Why a sync failed. Nothing is written when it does.
#[derive(Debug)]
#[non_exhaustive]
pub enum IMessageError {
    /// macOS refused to let Knode read the Messages folder.
    NoFullDiskAccess(PathBuf),
    /// There is no Messages database.
    NotFound(PathBuf),
    /// The database is not the one Knode knows: these tables or columns are missing.
    NeedsUpdate {
        /// `table` or `table.column`, for each one missing.
        missing: Vec<String>,
    },
    /// The copy could not be made.
    Io(io::Error),
    /// The copy could not be read.
    Sqlite(rusqlite::Error),
    /// The space could not be written.
    Write(ChatImportError),
}

impl IMessageError {
    fn from_open(path: &Path, error: io::Error) -> IMessageError {
        match error.kind() {
            io::ErrorKind::PermissionDenied => IMessageError::NoFullDiskAccess(path.to_path_buf()),
            io::ErrorKind::NotFound => IMessageError::NotFound(path.to_path_buf()),
            _ => IMessageError::Io(error),
        }
    }
}

impl fmt::Display for IMessageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IMessageError::NoFullDiskAccess(_) => f.write_str(
                "Knode cannot read your messages: macOS only lets apps with Full Disk Access \
                 read them. Turn on Knode in System Settings > Privacy & Security > Full Disk \
                 Access, then try again",
            ),
            IMessageError::NotFound(path) => write!(
                f,
                "there are no messages to read at {}: open Messages and sign in to iMessage \
                 first",
                path.display()
            ),
            IMessageError::NeedsUpdate { missing } => write!(
                f,
                "the iMessage connector needs an update: this version of Messages keeps its \
                 messages differently (missing {}). Nothing was read or written",
                missing.join(", ")
            ),
            IMessageError::Io(error) => {
                write!(f, "the Messages database could not be copied: {error}")
            }
            IMessageError::Sqlite(error) => {
                write!(f, "the copy of the Messages database could not be read: {error}")
            }
            IMessageError::Write(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for IMessageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IMessageError::Io(error) => Some(error),
            IMessageError::Sqlite(error) => Some(error),
            IMessageError::Write(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for IMessageError {
    fn from(error: io::Error) -> Self {
        IMessageError::Io(error)
    }
}

impl From<rusqlite::Error> for IMessageError {
    fn from(error: rusqlite::Error) -> Self {
        IMessageError::Sqlite(error)
    }
}

impl From<ChatImportError> for IMessageError {
    fn from(error: ChatImportError) -> Self {
        IMessageError::Write(error)
    }
}

impl From<knode_format::Error> for IMessageError {
    fn from(error: knode_format::Error) -> Self {
        IMessageError::Write(ChatImportError::Space(error))
    }
}

/// A conversation in Messages, as listed to choose which to exclude.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    /// Its guid, which [`SyncOptions::excluded`] names.
    pub guid: String,
    /// Its title, as its node would have it.
    pub title: String,
    /// `personal` or `group`.
    pub kind: String,
    /// The others in it.
    pub participants: Vec<String>,
}

/// What [`sync`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// The nodes written and what was in them, as for a chat export.
    pub chats: ChatImportReport,
    /// Where to go on from next time.
    pub cursor: Cursor,
    /// Conversations excluded, not read.
    pub excluded: usize,
    /// Reactions (tapbacks) left out.
    pub reactions: usize,
    /// Messages with nothing to show, such as group events other than a new name, left out.
    pub skipped_events: usize,
}

/// Every conversation in Messages, to choose which to exclude.
pub fn conversations(
    source: &MessagesSource,
    names: Option<&dyn ContactNames>,
) -> Result<Vec<Conversation>, IMessageError> {
    let snapshot = Snapshot::take(&source.database())?;
    Ok(snapshot
        .chats()?
        .iter()
        .map(|chat| {
            let participants = people(chat, names);
            Conversation {
                guid: chat.guid.clone(),
                title: title(chat, &participants),
                kind: kind(chat).to_string(),
                participants,
            }
        })
        .collect())
}

/// Where the last sync into `space` stopped, from the iMessage nodes in it: the highest message
/// number they were written up to. A sync that lost its cursor goes on from here.
pub fn cursor_in(space: &Space) -> Result<Option<Cursor>, IMessageError> {
    let mut best = None;
    for summary in space.find_by_property(keys::APP, APP)? {
        if let Some(last) = space.property(&summary.id, keys::LAST)?
            && let Ok(cursor) = last.parse::<Cursor>()
        {
            best = best.max(Some(cursor));
        }
    }
    Ok(best)
}

/// Brings the messages Messages has after `cursor` into `space`, written by `by`: one
/// [`CHAT_KIND`](super::CHAT_KIND) node per conversation, its new messages added under their
/// day, and the files sent attached. Without a cursor (the first sync), `options.history` says
/// where to start. The returned [`SyncReport::cursor`] is where the next sync goes on from; a
/// sync with a cursor older than what the space holds never writes a message twice.
///
/// The database is read from a copy, and checked before anything is read: an error writes
/// nothing.
pub fn sync(
    space: &mut Space,
    source: &MessagesSource,
    cursor: Option<Cursor>,
    options: &SyncOptions<'_>,
    by: &Provenance,
) -> Result<SyncReport, IMessageError> {
    let snapshot = Snapshot::take(&source.database())?;
    let newest = Cursor(snapshot.last_rowid()?);
    let cursor = cursor.max(cursor_in(space)?);
    let offset = options.offset;
    let (after, from) = match cursor {
        Some(cursor) => (cursor.0, None),
        None => match options.history {
            History::Everything => (0, None),
            History::FromToday => (0, Some(start_of_today(options.now, offset))),
        },
    };

    let mut files = Files::unlisted(&source.dir);
    let mut writer = ChatWriter {
        space,
        by: by.clone().with_source(APP),
        report: ChatImportReport::new(ChatFormat::IMessage),
    };
    let mut report = SyncReport {
        chats: ChatImportReport::new(ChatFormat::IMessage),
        cursor: newest.max(Cursor(after)),
        excluded: 0,
        reactions: 0,
        skipped_events: 0,
    };
    for row in snapshot.chats()? {
        if options.excluded.contains(&row.guid) {
            report.excluded += 1;
            continue;
        }
        let rows = snapshot.messages(row.rowid, after, from)?;
        if rows.is_empty() {
            continue;
        }
        let participants = people(&row, options.names);
        let mut chat = Chat::new(
            ChatFormat::IMessage,
            row.guid.clone(),
            title(&row, &participants),
        );
        chat.kind = kind(&row).to_string();
        chat.participants = participants;
        chat.participants.push(options.me.clone());
        for message in rows {
            if (2000..4000).contains(&message.associated_type) {
                report.reactions += 1;
                continue;
            }
            match to_message(&snapshot, &message, &row, options, source)? {
                Some(m) => chat.messages.push(m),
                None => report.skipped_events += 1,
            }
        }
        writer.write(&chat, &mut files)?;
    }
    report.chats = writer.report;
    Ok(report)
}

/// The start of today in `offset`, in Apple nanoseconds.
fn start_of_today(now: Option<DateTime<Utc>>, offset: Option<FixedOffset>) -> i64 {
    let now = now.unwrap_or_else(Utc::now);
    let start = match offset {
        Some(offset) => {
            let day = now.with_timezone(&offset).date_naive();
            offset
                .from_local_datetime(&day.and_time(NaiveTime::MIN))
                .earliest()
                .map(|t| t.timestamp())
        }
        None => {
            let day = now.with_timezone(&Local).date_naive();
            Local
                .from_local_datetime(&day.and_time(NaiveTime::MIN))
                .earliest()
                .map(|t| t.timestamp())
        }
    }
    .unwrap_or_else(|| now.timestamp());
    (start - APPLE_EPOCH).saturating_mul(1_000_000_000)
}

/// An Apple time as a time of day in `offset` (the Mac's when `None`).
fn local_time(date: i64, offset: Option<FixedOffset>) -> NaiveDateTime {
    // Before macOS 10.13 times were in seconds; since then, in nanoseconds.
    let (secs, nanos) = if date.abs() >= 1_000_000_000_000 {
        (date.div_euclid(1_000_000_000), date.rem_euclid(1_000_000_000))
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
    format!("iMessage - {}", name.split_whitespace().collect::<Vec<_>>().join(" "))
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
    text.replace(['\u{FFFC}', '\u{FFFD}'], "").trim().to_string()
}

fn sender(message: &MessageRow, chat: &ChatRow, options: &SyncOptions<'_>) -> String {
    if message.from_me {
        return options.me.clone();
    }
    match &message.handle {
        Some(handle) if !handle.is_empty() => person(handle, options.names),
        _ => match chat.handles.as_slice() {
            [only] => person(only, options.names),
            _ => "Unknown".to_string(),
        },
    }
}

fn to_message(
    snapshot: &Snapshot,
    message: &MessageRow,
    chat: &ChatRow,
    options: &SyncOptions<'_>,
    source: &MessagesSource,
) -> Result<Option<ChatMessage>, IMessageError> {
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
        .map(|a| attachment(a, source))
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
        && let Some(original) = snapshot.message_by_guid(guid)?
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
fn attachment(row: &db::AttachmentRow, source: &MessagesSource) -> Media {
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
    let Some(entry) = entry_in(path, &source.dir) else {
        return not_attached("not on this Mac");
    };
    let mut full = source.dir.clone();
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
    fn today_starts_at_midnight_where_the_person_is() {
        let now = DateTime::parse_from_rfc3339("2026-03-04T09:15:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let utc = FixedOffset::east_opt(0);
        assert_eq!(
            local_time(start_of_today(Some(now), utc), utc).to_string(),
            "2026-03-04 00:00:00"
        );
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
        assert_eq!(
            title(&chat, &people(&chat, None)),
            "iMessage - +15550100"
        );
        chat.display_name = "Trip [2026]".into();
        assert_eq!(title(&chat, &[]), "iMessage - Trip 2026");
    }
}
