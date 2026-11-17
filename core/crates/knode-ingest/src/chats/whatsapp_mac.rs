//! WhatsApp for Mac, read live: the chats the app keeps on the Mac, brought into a space as the
//! same nodes a WhatsApp export gives.
//!
//! WhatsApp for Mac keeps its chats in a SQLite database, [`DATABASE`], in its group container
//! ([`CONTAINER`], under the home folder), and the files sent in them under `Message/Media` beside
//! it. The database is the app's own and its format is not documented, so it is handled with
//! care:
//!
//! - **Never the live file, never written.** The database is opened read-only, and SQLite's
//!   backup API copies it, what its write-ahead log holds included, into a private temporary
//!   file, which is read and then deleted. The app is never locked out or written to.
//! - **The schema is checked first.** The tables and columns read ([`EXPECTED`]) must all be
//!   there; when one is not, nothing is written and the error says the connector needs an
//!   update ([`WhatsAppMacError::NeedsUpdate`]).
//! - **Only what is new.** A [`Cursor`] remembers the last message read (its primary key,
//!   `Z_PK`); each sync reads only the messages after it. The cursor is kept per space by
//!   whoever runs the sync.
//! - **Everything, except what is excluded.** Every chat comes in, new ones included, but the
//!   chats whose JID is in [`Options::excluded`]: their messages are never read.
//! - **History is chosen once.** The first sync reads the messages from the start of today
//!   ([`History::FromToday`]) or every message on the Mac ([`History::Everything`]).
//!
//! Each chat is written by the WhatsApp importer: one `chat` node per chat, its messages by day.
//! The node carries the chat's JID ([`keys::JID`](super::keys::JID)) and is found by it; a chat
//! imported from an export first, which has no JID, is found by its name and given the JID, so
//! an export and the connector write to the same node, either one first.
//!
//! [`WhatsAppMac::sync`] reads and writes in one go; [`WhatsAppMacConnector`] is the same as a
//! [`Connector`](crate::sources::Connector) of Knode Service, which keeps the cursor, the history
//! and the exclusions of each space and runs it when the database changes.
//!
//! Only what the phone synced to the Mac is there, and disappearing messages are gone. Reading
//! the database needs Full Disk Access for the process that reads it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use chrono::{Local, NaiveDateTime, TimeZone};
use knode_format::{Provenance, Space};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension, Row};

use super::files::Files;
use super::model::{Chat, ChatMessage, Media, stable_hash};
use super::render::{escape_name, escape_text};
use super::whatsapp::clean_title;
use super::write::ChatWriter;
use super::{ChatFormat, ChatImportError, ChatImportReport};
use crate::sources::sqlite::DatabaseCopy;

/// WhatsApp for Mac's group container, relative to the home folder.
pub const CONTAINER: &str = "Library/Group Containers/group.net.whatsapp.WhatsApp.shared";

/// The database in the container.
pub const DATABASE: &str = "ChatStorage.sqlite";

/// The folder in the container that the paths of the files sent in chats start from.
const MESSAGE_FOLDER: &str = "Message";

/// The tables and columns read. All of them must be there, or nothing is read.
pub const EXPECTED: &[(&str, &[&str])] = &[
    (
        "ZWACHATSESSION",
        &["Z_PK", "ZCONTACTJID", "ZPARTNERNAME", "ZGROUPINFO"],
    ),
    (
        "ZWAMESSAGE",
        &[
            "Z_PK",
            "ZCHATSESSION",
            "ZISFROMME",
            "ZMESSAGEDATE",
            "ZTEXT",
            "ZMESSAGETYPE",
            "ZGROUPMEMBER",
            "ZSTANZAID",
        ],
    ),
    (
        "ZWAMEDIAITEM",
        &["Z_PK", "ZMESSAGE", "ZMEDIALOCALPATH", "ZTITLE", "ZMETADATA"],
    ),
    ("ZWAGROUPMEMBER", &["Z_PK", "ZMEMBERJID"]),
    ("ZWAPROFILEPUSHNAME", &["ZJID", "ZPUSHNAME"]),
];

/// Seconds from 1970-01-01 to 2001-01-01, where Apple's clock starts.
const APPLE_EPOCH: i64 = 978_307_200;

/// The JID of WhatsApp's status updates, which are not a chat.
const STATUS_JID: &str = "status@broadcast";

/// What the first sync reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum History {
    /// The messages from the start of today, in the Mac's time zone. The default.
    #[default]
    FromToday,
    /// Every message on the Mac.
    Everything,
}

/// Where the last sync stopped: the primary key (`Z_PK`) of the last message in the database
/// then. Kept per space; [`Cursor::default`] has never synced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Cursor {
    last_message: Option<i64>,
}

impl Cursor {
    /// A cursor after the message with this primary key.
    pub fn after(last_message: i64) -> Self {
        Cursor {
            last_message: Some(last_message),
        }
    }

    /// The primary key of the last message read, or `None` before the first sync.
    pub fn last_message(&self) -> Option<i64> {
        self.last_message
    }
}

/// A cursor as text to keep: the number, or empty before the first sync.
impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.last_message {
            Some(n) => write!(f, "{n}"),
            None => Ok(()),
        }
    }
}

impl FromStr for Cursor {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Ok(Cursor::default());
        }
        s.parse().map(Cursor::after)
    }
}

/// How a sync reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// What the first sync reads; later ones read what is new.
    pub history: History,
    /// The JIDs of the chats never read.
    pub excluded: BTreeSet<String>,
    /// The name the person's own messages are shown with.
    pub me: String,
    /// With [`History::FromToday`], the start of the day the connector was connected, in
    /// milliseconds since 1970: nothing older is ever read. `None`: the start of today, for
    /// the first sync only.
    pub connected: Option<i64>,
    /// The most messages one read takes; the rest waits for the next ([`Pending::more`]).
    /// `None`: no limit.
    pub max_messages: Option<usize>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            history: History::FromToday,
            excluded: BTreeSet::new(),
            me: "Me".into(),
            connected: None,
            max_messages: None,
        }
    }
}

/// One chat on the Mac, for choosing what to exclude.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSummary {
    /// Its JID, which [`Options::excluded`] takes.
    pub jid: String,
    /// Its name, as WhatsApp shows it.
    pub name: String,
    /// Whether it is a group.
    pub group: bool,
}

/// What a sync did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// The chats written, as the importer reports them.
    pub import: ChatImportReport,
    /// The messages read from the database.
    pub read: usize,
    /// Where to start next time: keep it for the space.
    pub cursor: Cursor,
}

/// Messages read and not written yet: [`WhatsAppMac::read`], then [`Pending::write`].
#[derive(Debug, Clone)]
pub struct Pending {
    chats: Vec<Chat>,
    message_dir: PathBuf,
    source: String,
    read: usize,
    cursor: Cursor,
    more: bool,
}

impl Pending {
    /// The messages read from the database.
    pub fn messages(&self) -> usize {
        self.read
    }

    /// Where to start next time, once this is written.
    pub fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Whether there are more messages to read now ([`Options::max_messages`]).
    pub fn more(&self) -> bool {
        self.more
    }

    /// Writes the messages into `space` as `by`, through the WhatsApp importer, which adds
    /// nothing it wrote before: writing the same messages twice changes nothing.
    pub fn write(
        self,
        space: &mut Space,
        by: &Provenance,
    ) -> Result<ChatImportReport, ChatImportError> {
        let mut writer = ChatWriter {
            space,
            by: by.clone().with_source(self.source),
            report: ChatImportReport::new(ChatFormat::WhatsApp),
        };
        if self.chats.is_empty() {
            return Ok(writer.report);
        }
        // Files are opened by their path in the Message folder; when it is gone, every one is
        // reported as not attached.
        let mut files = Files::unlisted_folder(&self.message_dir)
            .or_else(|_| Files::unlisted_folder(&std::env::temp_dir()))?;
        for chat in &self.chats {
            writer.write(chat, &mut files)?;
        }
        Ok(writer.report)
    }
}

/// Numbers the export keys that repeat within a minute (`-1`, `-2`...), as an export does.
fn number_export_keys(messages: &mut [ChatMessage]) {
    let mut seen: BTreeMap<(String, String), usize> = BTreeMap::new();
    for message in messages {
        if let Some(base) = message.export_key.take() {
            let n = seen
                .entry((message.mark.clone(), base.clone()))
                .or_insert(0);
            message.export_key = Some(if *n == 0 { base } else { format!("{base}-{n}") });
            *n += 1;
        }
    }
}

/// Why WhatsApp for Mac could not be read.
#[derive(Debug)]
#[non_exhaustive]
pub enum WhatsAppMacError {
    /// WhatsApp for Mac's database is not there: the app is not installed, or not linked.
    NotInstalled(PathBuf),
    /// The database is there, but this process may not read it.
    NoFullDiskAccess(PathBuf),
    /// The database does not have the tables and columns expected: WhatsApp changed it.
    /// Nothing was written.
    NeedsUpdate {
        /// The tables, and `table.column`s, missing.
        missing: Vec<String>,
    },
    /// The file is not a database WhatsApp for Mac writes, or its copy could not be opened.
    NotADatabase(String),
    /// The database could not be copied or read.
    Database(rusqlite::Error),
    /// A file could not be read or written.
    Io(io::Error),
    /// The chats could not be written into the space.
    Import(ChatImportError),
}

impl fmt::Display for WhatsAppMacError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WhatsAppMacError::NotInstalled(path) => write!(
                f,
                "WhatsApp for Mac was not found ({} does not exist): install WhatsApp from the \
                 App Store, link it to your phone, and connect again",
                path.display()
            ),
            WhatsAppMacError::NoFullDiskAccess(path) => write!(
                f,
                "Knode may not read WhatsApp for Mac's chats ({}): give it Full Disk Access in \
                 System Settings > Privacy & Security > Full Disk Access, and connect again",
                path.display()
            ),
            WhatsAppMacError::NeedsUpdate { missing } => write!(
                f,
                "WhatsApp for Mac changed how it keeps its chats ({} not found), so this \
                 connector needs an update. Nothing was written",
                missing.join(", ")
            ),
            WhatsAppMacError::NotADatabase(said) => write!(
                f,
                "WhatsApp for Mac's chats could not be read ({said}), so this connector needs an \
                 update. Nothing was written"
            ),
            WhatsAppMacError::Database(error) => {
                write!(f, "WhatsApp for Mac's chats could not be read: {error}")
            }
            WhatsAppMacError::Io(error) => {
                write!(f, "WhatsApp for Mac's chats could not be read: {error}")
            }
            WhatsAppMacError::Import(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for WhatsAppMacError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WhatsAppMacError::Database(error) => Some(error),
            WhatsAppMacError::Io(error) => Some(error),
            WhatsAppMacError::Import(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for WhatsAppMacError {
    fn from(error: rusqlite::Error) -> Self {
        WhatsAppMacError::Database(error)
    }
}

impl From<io::Error> for WhatsAppMacError {
    fn from(error: io::Error) -> Self {
        WhatsAppMacError::Io(error)
    }
}

impl From<ChatImportError> for WhatsAppMacError {
    fn from(error: ChatImportError) -> Self {
        WhatsAppMacError::Import(error)
    }
}

impl From<knode_format::Error> for WhatsAppMacError {
    fn from(error: knode_format::Error) -> Self {
        WhatsAppMacError::Import(ChatImportError::Space(error))
    }
}

/// WhatsApp for Mac's chats, in its container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhatsAppMac {
    container: PathBuf,
}

impl WhatsAppMac {
    /// The app's container at `container` (the folder [`DATABASE`] is in).
    pub fn new(container: impl Into<PathBuf>) -> Self {
        WhatsAppMac {
            container: container.into(),
        }
    }

    /// The container in a home folder: [`CONTAINER`] in it.
    pub fn for_home(home: impl AsRef<Path>) -> Self {
        WhatsAppMac::new(home.as_ref().join(CONTAINER))
    }

    /// The container.
    pub fn container(&self) -> &Path {
        &self.container
    }

    /// The database.
    pub fn database(&self) -> PathBuf {
        self.container.join(DATABASE)
    }

    /// The files the app writes when a message arrives, for a watcher: the database and its
    /// write-ahead log.
    pub fn watched_files(&self) -> Vec<PathBuf> {
        let database = self.database();
        let mut wal = database.clone().into_os_string();
        wal.push("-wal");
        vec![database, PathBuf::from(wal)]
    }

    /// Checks that the database is there, may be read and has the tables and columns read.
    pub fn check(&self) -> Result<(), WhatsAppMacError> {
        self.copy().map(drop)
    }

    /// Every chat on the Mac, by name.
    pub fn chats(&self) -> Result<Vec<ChatSummary>, WhatsAppMacError> {
        let copy = self.copy()?;
        let names = Names::read(copy.conn())?;
        let mut chats: Vec<ChatSummary> = names
            .sessions
            .values()
            .map(|s| ChatSummary {
                jid: s.jid.clone(),
                name: names.chat_name(s),
                group: s.group,
            })
            .collect();
        chats.sort_by(|a, b| (a.name.to_lowercase(), &a.jid).cmp(&(b.name.to_lowercase(), &b.jid)));
        Ok(chats)
    }

    /// Brings the messages after `cursor` into `space`, written by `by`, and says where to
    /// start next time: [`read`](Self::read), then [`Pending::write`]. Nothing is written when
    /// the database cannot be read or its schema is not the one expected.
    pub fn sync(
        &self,
        space: &mut Space,
        cursor: Cursor,
        options: &Options,
        by: &Provenance,
    ) -> Result<SyncReport, WhatsAppMacError> {
        let pending = self.read(cursor, options)?;
        let read = pending.messages();
        let cursor = pending.cursor();
        let import = pending.write(space, by)?;
        Ok(SyncReport {
            import,
            read,
            cursor,
        })
    }

    /// Reads the messages after `cursor`, ready to be written, without touching a space. The
    /// copy of the database is gone when this returns.
    pub fn read(&self, cursor: Cursor, options: &Options) -> Result<Pending, WhatsAppMacError> {
        let copy = self.copy()?;
        let connection = &copy.conn();
        let names = Names::read(connection)?;

        let newest: Option<i64> =
            connection.query_row("SELECT MAX(Z_PK) FROM ZWAMESSAGE", [], |row| row.get(0))?;
        let included: Vec<i64> = names
            .sessions
            .values()
            .filter(|s| !options.excluded.contains(&s.jid))
            .map(|s| s.pk)
            .collect();
        let since = match (options.history, options.connected, cursor.last_message) {
            (History::Everything, _, _) => None,
            (History::FromToday, Some(ms), _) => Some((ms.div_euclid(1000) - APPLE_EPOCH) as f64),
            (History::FromToday, None, None) => Some(start_of_today()),
            (History::FromToday, None, Some(_)) => None,
        };
        let limit = options.max_messages.map(|n| n.max(1));
        let mut rows = if included.is_empty() {
            Vec::new()
        } else {
            read_rows(
                connection,
                cursor.last_message.unwrap_or(i64::MIN),
                since,
                &included,
                limit.map(|n| n + 1),
            )?
        };
        let more = limit.is_some_and(|n| rows.len() > n);
        if let Some(n) = limit {
            rows.truncate(n);
        }
        // Past the last message read when there is more; else past every message there is,
        // those of excluded chats too.
        let last = if more {
            rows.last().map(|r| r.pk)
        } else {
            newest
        };
        let next = Cursor {
            last_message: match (cursor.last_message, last) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b).or(Some(0)),
            },
        };
        let read = rows.len();

        let message_dir = self.container.join(MESSAGE_FOLDER);
        let mut chats: BTreeMap<i64, Chat> = BTreeMap::new();
        let mut order: BTreeMap<i64, Vec<(NaiveDateTime, i64)>> = BTreeMap::new();
        let mut replies = Replies::new(connection)?;
        for row in rows {
            let Some(session) = names.sessions.get(&row.session) else {
                continue;
            };
            let Some(message) =
                to_message(&row, session, &names, options, &message_dir, &mut replies)?
            else {
                continue;
            };
            let chat = chats.entry(session.pk).or_insert_with(|| {
                let name = names.chat_name(session);
                let mut chat = Chat::new(
                    ChatFormat::WhatsApp,
                    name.to_lowercase(),
                    format!("WhatsApp - {name}"),
                );
                chat.jid = Some(session.jid.clone());
                chat.kind = if session.group { "group" } else { "personal" }.into();
                chat
            });
            order
                .entry(session.pk)
                .or_default()
                .push((message.time, row.pk));
            chat.messages.push(message);
        }
        let mut chats: Vec<Chat> = chats
            .into_iter()
            .map(|(pk, mut chat)| {
                // Oldest first, as the importer takes them.
                let keys = order.remove(&pk).unwrap_or_default();
                let mut paired: Vec<_> = keys.into_iter().zip(chat.messages).collect();
                paired.sort_by_key(|(key, _)| *key);
                chat.messages = paired.into_iter().map(|(_, m)| m).collect();
                number_export_keys(&mut chat.messages);
                chat.collect_participants();
                chat
            })
            .collect();
        chats.sort_by(|a, b| a.title.cmp(&b.title));
        Ok(Pending {
            chats,
            message_dir,
            source: self.database().to_string_lossy().into_owned(),
            read,
            cursor: next,
            more,
        })
    }

    /// Checks that the database is there and this process may read it, without copying it.
    /// The schema is checked by every read, before anything is written.
    pub fn available(&self) -> Result<(), WhatsAppMacError> {
        let database = self.database();
        for path in [self.container.as_path(), database.as_path()] {
            let found = if path == database {
                fs::File::open(path).map(drop)
            } else {
                fs::metadata(path).map(drop)
            };
            match found {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    return Err(WhatsAppMacError::NotInstalled(database.clone()));
                }
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                    return Err(WhatsAppMacError::NoFullDiskAccess(database.clone()));
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// A private copy of the database, checked. The database and its `-wal` and `-shm` files
    /// are copied as bytes (see [`sqlite::open_copy`](crate::sources::sqlite::open_copy)):
    /// SQLite never opens the live files, so nothing is created, locked or written beside
    /// them.
    fn copy(&self) -> Result<DatabaseCopy, WhatsAppMacError> {
        use crate::sources::SourceError;
        self.available()?;
        let database = self.database();
        let copy = crate::sources::sqlite::open_copy(&database).map_err(|e| match e {
            SourceError::Unavailable(_) => WhatsAppMacError::NotInstalled(database.clone()),
            SourceError::Permission(..) => WhatsAppMacError::NoFullDiskAccess(database.clone()),
            SourceError::NeedsUpdate(said) => WhatsAppMacError::NotADatabase(said),
            other => WhatsAppMacError::Io(io::Error::other(other.to_string())),
        })?;
        check_schema(copy.conn())?;
        Ok(copy)
    }
}

/// Fails with [`WhatsAppMacError::NeedsUpdate`] unless every table and column of [`EXPECTED`]
/// is there.
fn check_schema(connection: &Connection) -> Result<(), WhatsAppMacError> {
    let mut missing = Vec::new();
    let mut statement = connection.prepare("SELECT name FROM pragma_table_info(?1)")?;
    for (table, columns) in EXPECTED {
        let have = statement
            .query_map([table], |row| row.get::<_, String>(0))?
            .collect::<Result<BTreeSet<_>, _>>()?;
        if have.is_empty() {
            missing.push((*table).to_string());
            continue;
        }
        for column in *columns {
            if !have.contains(*column) {
                missing.push(format!("{table}.{column}"));
            }
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(WhatsAppMacError::NeedsUpdate { missing })
    }
}

/// One chat in the database.
#[derive(Debug, Clone)]
struct Session {
    pk: i64,
    jid: String,
    name: Option<String>,
    group: bool,
}

/// The chats, and the names people go by.
struct Names {
    sessions: BTreeMap<i64, Session>,
    /// The name of a person's one-to-one chat, by their JID.
    partners: BTreeMap<String, String>,
    /// The name people gave themselves, by JID.
    push: BTreeMap<String, String>,
}

impl Names {
    fn read(connection: &Connection) -> Result<Names, WhatsAppMacError> {
        let mut sessions = BTreeMap::new();
        let mut partners = BTreeMap::new();
        let mut statement = connection.prepare(
            "SELECT Z_PK, ZCONTACTJID, ZPARTNERNAME, ZGROUPINFO FROM ZWACHATSESSION \
             WHERE ZCONTACTJID IS NOT NULL",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let Some(jid) = text(row, 1) else { continue };
            if jid == STATUS_JID {
                continue;
            }
            let group = !matches!(row.get_ref(3)?, ValueRef::Null) || jid.ends_with("@g.us");
            let name = text(row, 2)
                .map(|n| clean_title(&n))
                .filter(|n| !n.is_empty());
            if !group && let Some(name) = &name {
                partners.insert(jid.clone(), name.clone());
            }
            let pk: i64 = row.get(0)?;
            sessions.insert(
                pk,
                Session {
                    pk,
                    jid,
                    name,
                    group,
                },
            );
        }
        let mut push = BTreeMap::new();
        let mut statement = connection.prepare(
            "SELECT ZJID, ZPUSHNAME FROM ZWAPROFILEPUSHNAME \
             WHERE ZJID IS NOT NULL AND ZPUSHNAME IS NOT NULL",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            if let (Some(jid), Some(name)) = (text(row, 0), text(row, 1)) {
                let name = clean_title(&name);
                if !name.is_empty() {
                    push.insert(jid, name);
                }
            }
        }
        Ok(Names {
            sessions,
            partners,
            push,
        })
    }

    /// The name of a chat: its name in WhatsApp, else the person's, else their number.
    fn chat_name(&self, session: &Session) -> String {
        match &session.name {
            Some(name) if !session.group && is_number(name) => self
                .push
                .get(&session.jid)
                .cloned()
                .unwrap_or_else(|| name.clone()),
            Some(name) => name.clone(),
            None => self.person(&session.jid),
        }
    }

    /// The name of a person: their chat's name, the name they gave themselves, or their number.
    fn person(&self, jid: &str) -> String {
        if let Some(name) = self.partners.get(jid)
            && !is_number(name)
        {
            return name.clone();
        }
        if let Some(name) = self.push.get(jid) {
            return name.clone();
        }
        let user = jid.split('@').next().unwrap_or(jid);
        if !user.is_empty() && user.chars().all(|c| c.is_ascii_digit()) {
            format!("+{user}")
        } else {
            user.to_string()
        }
    }
}

/// Whether a name is only a phone number.
fn is_number(name: &str) -> bool {
    let digits: String = name
        .chars()
        .filter(|c| !matches!(c, '+' | ' ' | '-' | '(' | ')'))
        .collect();
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

/// One message as the database holds it.
struct Stored {
    pk: i64,
    session: i64,
    from_me: bool,
    date: f64,
    text: Option<String>,
    kind: i64,
    member: Option<String>,
    stanza: Option<String>,
    media_path: Option<String>,
    media_title: Option<String>,
    metadata: Option<Vec<u8>>,
}

fn read_rows(
    connection: &Connection,
    after: i64,
    since: Option<f64>,
    sessions: &[i64],
    limit: Option<usize>,
) -> Result<Vec<Stored>, WhatsAppMacError> {
    // The chat keys are integers from the database itself.
    let list = sessions
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    // -1 is no limit.
    let limit = limit.map_or(-1, |n| i64::try_from(n).unwrap_or(i64::MAX));
    let sql = format!(
        "SELECT m.Z_PK, m.ZCHATSESSION, m.ZISFROMME, m.ZMESSAGEDATE, m.ZTEXT, m.ZMESSAGETYPE, \
                g.ZMEMBERJID, m.ZSTANZAID, \
                (SELECT i.ZMEDIALOCALPATH FROM ZWAMEDIAITEM i WHERE i.ZMESSAGE = m.Z_PK \
                 ORDER BY i.Z_PK LIMIT 1), \
                (SELECT i.ZTITLE FROM ZWAMEDIAITEM i WHERE i.ZMESSAGE = m.Z_PK \
                 ORDER BY i.Z_PK LIMIT 1), \
                (SELECT i.ZMETADATA FROM ZWAMEDIAITEM i WHERE i.ZMESSAGE = m.Z_PK \
                 ORDER BY i.Z_PK LIMIT 1) \
         FROM ZWAMESSAGE m LEFT JOIN ZWAGROUPMEMBER g ON m.ZGROUPMEMBER = g.Z_PK \
         WHERE m.Z_PK > ?1 AND m.ZMESSAGEDATE IS NOT NULL AND m.ZMESSAGEDATE >= ?2 \
           AND m.ZCHATSESSION IN ({list}) \
         ORDER BY m.Z_PK LIMIT {limit}"
    );
    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement.query(rusqlite::params![after, since.unwrap_or(f64::MIN)])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(Stored {
            pk: row.get(0)?,
            session: row.get(1)?,
            from_me: row.get::<_, Option<i64>>(2)?.unwrap_or(0) != 0,
            date: row.get(3)?,
            text: text(row, 4),
            kind: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
            member: text(row, 6),
            stanza: text(row, 7),
            media_path: text(row, 8).filter(|p| !p.is_empty()),
            media_title: text(row, 9).filter(|t| !t.trim().is_empty()),
            metadata: match row.get_ref(10)? {
                ValueRef::Blob(b) => Some(b.to_vec()),
                _ => None,
            },
        });
    }
    Ok(out)
}

/// Message types, as WhatsApp numbers them in `ZMESSAGETYPE`.
mod kind {
    pub const IMAGE: i64 = 1;
    pub const VIDEO: i64 = 2;
    pub const AUDIO: i64 = 3;
    pub const CONTACT: i64 = 4;
    pub const LOCATION: i64 = 5;
    pub const SYSTEM: i64 = 6;
    pub const DOCUMENT: i64 = 8;
    pub const GROUP_EVENT: i64 = 10;
    pub const GIF: i64 = 11;
    pub const DELETED: i64 = 14;
    pub const STICKER: i64 = 15;
}

/// What a file of this type is called when the Mac does not have it.
fn media_word(kind: i64) -> Option<&'static str> {
    Some(match kind {
        kind::IMAGE => "photo",
        kind::VIDEO => "video",
        kind::AUDIO => "audio",
        kind::DOCUMENT => "document",
        kind::GIF => "GIF",
        kind::STICKER => "sticker",
        _ => return None,
    })
}

/// A message of the database as the importer takes it, or `None` for one with nothing to show.
fn to_message(
    row: &Stored,
    session: &Session,
    names: &Names,
    options: &Options,
    message_dir: &Path,
    replies: &mut Replies<'_>,
) -> Result<Option<ChatMessage>, WhatsAppMacError> {
    let Some(time) = local_time(row.date) else {
        return Ok(None);
    };
    let system = matches!(row.kind, kind::SYSTEM | kind::GROUP_EVENT);
    let sender = if system {
        None
    } else if row.from_me {
        Some(options.me.clone())
    } else if session.group {
        row.member.as_ref().map(|member| names.person(member))
    } else {
        Some(names.chat_name(session))
    };

    let mut text = row.text.clone().unwrap_or_default();
    let mut media = Vec::new();
    if system {
        // A group's new name is the only change written as text; the rest are numbers.
        let name = text.trim();
        if !session.group || name.is_empty() || name.parse::<i64>().is_ok() {
            return Ok(None);
        }
        text = format!("The group's name changed to {name}");
    } else if row.kind == kind::DELETED {
        text = "This message was deleted".into();
    } else {
        let word = media_word(row.kind);
        if word.is_some() && text.trim().is_empty() && row.kind != kind::DOCUMENT {
            // A photo's or a video's caption.
            text = row.media_title.clone().unwrap_or_default();
        }
        let name = |path: &str| {
            let file = path.rsplit('/').next().unwrap_or(path).to_string();
            match (&row.media_title, row.kind) {
                (Some(title), kind::DOCUMENT) if title.contains('.') => title.clone(),
                _ => file,
            }
        };
        match (&row.media_path, word) {
            (Some(path), _) => match media_entry(message_dir, path) {
                Some(entry) => media.push(Media::File {
                    name: name(path),
                    entry,
                }),
                None => media.push(Media::NotOnDevice { name: name(path) }),
            },
            (None, Some(word)) => media.push(Media::NotOnDevice {
                name: match (&row.media_title, row.kind) {
                    (Some(title), kind::DOCUMENT) => title.clone(),
                    _ => word.to_string(),
                },
            }),
            (None, None) => {}
        }
        if text.trim().is_empty() && media.is_empty() {
            match row.kind {
                kind::CONTACT => text = "Contact card".into(),
                kind::LOCATION => text = "Location".into(),
                _ => return Ok(None),
            }
        }
    }

    let mark = time.format("%Y-%m-%dT%H:%M").to_string();
    let text = text.replace("\r\n", "\n");
    let key = match &row.stanza {
        Some(stanza) => format!("wa-{stanza}"),
        None => stable_hash(&[&mark, sender.as_deref().unwrap_or(""), &text]),
    };
    // The key a WhatsApp export gives the same text message: its minute, sender and line.
    let raw = match &sender {
        Some(sender) => format!("{sender}: {text}"),
        None => text.clone(),
    };
    let export_key = stable_hash(&[&mark, sender.as_deref().unwrap_or(""), &raw]);
    let mut message = ChatMessage::new(key, mark, time, sender, escape_text(text.trim()));
    message.export_key = Some(export_key);
    message.seq = Some(row.pk);
    message.media = media;
    if !system && let Some(quoted) = quoted_id(row.metadata.as_deref()) {
        message.reply_to = replies.find(row.session, &quoted, session, names, options)?;
    }
    Ok(Some(message))
}

/// The path of a file inside the `Message` folder, when the Mac has it. Paths are read as
/// relative to `Message` (`Media/<chat>/...`), or to `Message/Media`; one that leaves the folder
/// is never followed.
fn media_entry(message_dir: &Path, path: &str) -> Option<String> {
    let relative = Path::new(path);
    if !relative
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
    {
        return None;
    }
    let path = path.trim_start_matches("./");
    [path.to_string(), format!("Media/{path}")]
        .into_iter()
        .find(|entry| message_dir.join(entry).is_file())
}

/// The id of the message a reply quotes, from its media item's metadata: `0x2a 0x14` and then
/// the first 17 characters of the quoted message's id.
fn quoted_id(metadata: Option<&[u8]>) -> Option<String> {
    let metadata = metadata?;
    if metadata.len() < 19 || metadata[..2] != [0x2a, 0x14] {
        return None;
    }
    std::str::from_utf8(&metadata[2..19])
        .ok()
        .map(str::to_string)
}

/// Finds the messages replies quote.
struct Replies<'c> {
    connection: &'c Connection,
}

impl<'c> Replies<'c> {
    fn new(connection: &'c Connection) -> Result<Self, WhatsAppMacError> {
        Ok(Replies { connection })
    }

    /// The quoted message as people read it: `Ada at 09:15: "Shall we..."`.
    fn find(
        &mut self,
        session_pk: i64,
        quoted: &str,
        session: &Session,
        names: &Names,
        options: &Options,
    ) -> Result<Option<String>, WhatsAppMacError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT m.ZISFROMME, m.ZMESSAGEDATE, m.ZTEXT, g.ZMEMBERJID \
             FROM ZWAMESSAGE m LEFT JOIN ZWAGROUPMEMBER g ON m.ZGROUPMEMBER = g.Z_PK \
             WHERE m.ZCHATSESSION = ?1 AND substr(m.ZSTANZAID, 1, 17) = ?2 LIMIT 1",
        )?;
        let found = statement
            .query_row(rusqlite::params![session_pk, quoted], |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?.unwrap_or(0) != 0,
                    row.get::<_, Option<f64>>(1)?,
                    text(row, 2),
                    text(row, 3),
                ))
            })
            .optional()?;
        let Some((from_me, date, quoted_text, member)) = found else {
            return Ok(None);
        };
        let who = if from_me {
            options.me.clone()
        } else if session.group {
            member.map_or_else(|| "someone".to_string(), |m| names.person(&m))
        } else {
            names.chat_name(session)
        };
        let mut said = format!("{} ", escape_name(&who));
        if let Some(time) = date.and_then(local_time) {
            said.push_str(&format!("at {}", time.format("%H:%M")));
        }
        if let Some(t) = quoted_text.filter(|t| !t.trim().is_empty()) {
            let line = crate::text::one_line(&t, 60);
            said.push_str(&format!(": \"{}\"", escape_text(&line)));
        }
        Ok(Some(said.trim_end().to_string()))
    }
}

/// A time in Apple's clock (seconds since 2001-01-01, UTC) in the Mac's time zone.
fn local_time(seconds: f64) -> Option<NaiveDateTime> {
    if !seconds.is_finite() {
        return None;
    }
    let whole = seconds.floor();
    let nanos = ((seconds - whole) * 1e9) as u32;
    let unix = APPLE_EPOCH.checked_add(whole as i64)?;
    Local
        .timestamp_opt(unix, nanos.min(999_999_999))
        .earliest()
        .map(|t| t.naive_local())
}

/// The start of today in the Mac's time zone, in Apple's clock.
fn start_of_today() -> f64 {
    let today = Local::now().date_naive();
    let midnight = today.and_hms_opt(0, 0, 0).expect("midnight is a time");
    let unix = Local
        .from_local_datetime(&midnight)
        .earliest()
        .map(|t| t.timestamp())
        .unwrap_or_else(|| Local::now().timestamp() - 86_400);
    (unix - APPLE_EPOCH) as f64
}

/// A column stored as text, or as a blob holding UTF-8 text.
fn text(row: &Row<'_>, index: usize) -> Option<String> {
    match row.get_ref(index).ok()? {
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => String::from_utf8(bytes.to_vec()).ok(),
        _ => None,
    }
}

/// The id of the source: `whatsapp-mac` (`whatsapp` is the export importer's).
pub const SOURCE_ID: &str = "whatsapp-mac";

/// WhatsApp for Mac as a [`Connector`](crate::sources::Connector) of Knode Service: it runs
/// when the database or its write-ahead log changes, and at least every minute.
#[derive(Debug, Clone)]
pub struct WhatsAppMacConnector {
    app: WhatsAppMac,
    me: String,
}

impl WhatsAppMacConnector {
    /// The connector for the app in `app`, showing the person's own messages as `me`.
    pub fn new(app: WhatsAppMac, me: impl Into<String>) -> Self {
        WhatsAppMacConnector { app, me: me.into() }
    }

    /// The app it reads.
    pub fn app(&self) -> &WhatsAppMac {
        &self.app
    }
}

impl From<WhatsAppMacError> for crate::sources::SourceError {
    fn from(error: WhatsAppMacError) -> Self {
        use crate::sources::{Permission, SourceError};
        let said = error.to_string();
        match error {
            WhatsAppMacError::NotInstalled(_) => SourceError::Unavailable(said),
            WhatsAppMacError::NoFullDiskAccess(_) => {
                SourceError::Permission(Permission::FullDiskAccess, said)
            }
            WhatsAppMacError::NeedsUpdate { .. } | WhatsAppMacError::NotADatabase(_) => {
                SourceError::NeedsUpdate(said)
            }
            WhatsAppMacError::Import(ChatImportError::Space(error)) => SourceError::Space(error),
            _ => SourceError::Failed(said),
        }
    }
}

impl crate::sources::Connector for WhatsAppMacConnector {
    fn info(&self) -> crate::sources::SourceInfo {
        use crate::sources::{Permission, SourceInfo};
        SourceInfo::connector(
            SOURCE_ID,
            "WhatsApp for Mac",
            "The chats of WhatsApp for Mac",
        )
        .needing([Permission::FullDiskAccess])
        .excluding("chat")
    }

    fn trigger(&self) -> crate::sources::Trigger {
        crate::sources::Trigger::Watch {
            paths: self.app.watched_files(),
            every: Duration::from_secs(60),
        }
    }

    fn check(&self) -> Result<(), crate::sources::SourceError> {
        // Only whether it is there and may be read: `fetch` copies the database and checks
        // its schema before reading, so a run copies it once.
        Ok(self.app.available()?)
    }

    fn containers(
        &self,
        _request: &crate::sources::FetchRequest<'_>,
    ) -> Result<Vec<crate::sources::Container>, crate::sources::SourceError> {
        Ok(self
            .app
            .chats()?
            .into_iter()
            .map(|chat| crate::sources::Container::new(chat.jid, chat.name))
            .collect())
    }

    fn fetch(
        &self,
        request: &crate::sources::FetchRequest<'_>,
    ) -> Result<crate::sources::Fetched, crate::sources::SourceError> {
        use crate::sources::{Fetched, SourceError};
        let cursor = match request.cursor {
            None => Cursor::default(),
            Some(saved) => saved.as_str().parse().map_err(|_| {
                SourceError::Failed(format!(
                    "the saved place of the WhatsApp connector, {:?}, is not one it wrote",
                    saved.as_str()
                ))
            })?,
        };
        let options = Options {
            history: match request.history {
                crate::sources::History::FromToday => History::FromToday,
                crate::sources::History::Everything => History::Everything,
            },
            excluded: request.excluded.iter().map(|c| c.id.clone()).collect(),
            me: self.me.clone(),
            connected: Some(request.since.0),
            max_messages: Some(request.max_items),
        };
        let pending = self.app.read(cursor, &options)?;
        let next = crate::sources::Cursor::new(pending.cursor().to_string());
        let more = pending.more();
        if pending.messages() == 0 {
            return Ok(Fetched::nothing(next).with_more(more));
        }
        Ok(Fetched::batch(Batch(pending), next).with_more(more))
    }
}

/// Messages read by the connector, written by the WhatsApp importer.
struct Batch(Pending);

impl crate::sources::Batch for Batch {
    fn len(&self) -> usize {
        self.0.messages()
    }

    fn write(
        self: Box<Self>,
        space: &mut Space,
        by: &Provenance,
    ) -> Result<crate::sources::Written, crate::sources::SourceError> {
        let report = self
            .0
            .write(space, by)
            .map_err(|e| crate::sources::SourceError::from(WhatsAppMacError::Import(e)))?;
        let mut written = crate::sources::Written {
            items: report.messages,
            ..Default::default()
        };
        for chat in &report.chats {
            match chat.op {
                knode_format::ChangeOp::Create => written.created += 1,
                _ => written.updated += 1,
            }
        }
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_apple_time() {
        let t = local_time(0.0).unwrap();
        let expected = Local.timestamp_opt(APPLE_EPOCH, 0).unwrap().naive_local();
        assert_eq!(t, expected);
        assert!(local_time(f64::NAN).is_none());
    }

    #[test]
    fn reads_quoted_ids() {
        let mut metadata = vec![0x2a, 0x14];
        metadata.extend_from_slice(b"3EB0ABCDEF0123456789");
        assert_eq!(
            quoted_id(Some(&metadata)).as_deref(),
            Some("3EB0ABCDEF0123456")
        );
        assert_eq!(quoted_id(Some(b"\x2a\x14short")), None);
        assert_eq!(quoted_id(None), None);
    }

    #[test]
    fn keeps_cursors_as_text() {
        assert_eq!(Cursor::default().to_string(), "");
        assert_eq!("".parse::<Cursor>().unwrap(), Cursor::default());
        assert_eq!("42".parse::<Cursor>().unwrap(), Cursor::after(42));
        assert_eq!(Cursor::after(42).to_string(), "42");
    }

    #[test]
    fn never_leaves_the_media_folder() {
        let dir = std::env::temp_dir();
        assert_eq!(media_entry(&dir, "../etc/passwd"), None);
        assert_eq!(media_entry(&dir, "/etc/passwd"), None);
    }

    #[test]
    fn tells_numbers_from_names() {
        assert!(is_number("+34 600 000 000"));
        assert!(!is_number("Ada"));
    }
}
