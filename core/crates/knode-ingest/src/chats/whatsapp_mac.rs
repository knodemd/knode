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
//! Only what the phone synced to the Mac is there, and disappearing messages are gone. Reading
//! the database needs Full Disk Access for the process that reads it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{Local, NaiveDateTime, TimeZone};
use knode_format::{Provenance, Space};
use rusqlite::backup::{Backup, StepResult};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row};

use super::files::Files;
use super::model::{Chat, ChatMessage, Media, stable_hash};
use super::render::{escape_name, escape_text};
use super::whatsapp::clean_title;
use super::write::ChatWriter;
use super::{ChatFormat, ChatImportError, ChatImportReport};

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
}

impl Default for Options {
    fn default() -> Self {
        Options {
            history: History::FromToday,
            excluded: BTreeSet::new(),
            me: "Me".into(),
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
        let names = Names::read(&copy.connection)?;
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
    /// start next time. Nothing is written when the database cannot be read or its schema is
    /// not the one expected.
    pub fn sync(
        &self,
        space: &mut Space,
        cursor: Cursor,
        options: &Options,
        by: &Provenance,
    ) -> Result<SyncReport, WhatsAppMacError> {
        let copy = self.copy()?;
        let connection = &copy.connection;
        let names = Names::read(connection)?;

        let newest: Option<i64> =
            connection.query_row("SELECT MAX(Z_PK) FROM ZWAMESSAGE", [], |row| row.get(0))?;
        let next = Cursor {
            last_message: match (cursor.last_message, newest) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b).or(Some(0)),
            },
        };

        let included: Vec<i64> = names
            .sessions
            .values()
            .filter(|s| !options.excluded.contains(&s.jid))
            .map(|s| s.pk)
            .collect();
        let since = match (cursor.last_message, options.history) {
            (None, History::FromToday) => Some(start_of_today()),
            _ => None,
        };
        let rows = if included.is_empty() {
            Vec::new()
        } else {
            read_rows(
                connection,
                cursor.last_message.unwrap_or(i64::MIN),
                since,
                &included,
            )?
        };
        let read = rows.len();

        let message_dir = self.container.join(MESSAGE_FOLDER);
        let mut files = if message_dir.is_dir() {
            Files::unlisted_folder(&message_dir)?
        } else {
            Files::unlisted_folder(copy.dir.path())?
        };
        let mut chats: BTreeMap<i64, Chat> = BTreeMap::new();
        let mut seen: BTreeMap<(i64, String, String), usize> = BTreeMap::new();
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
            let mut message = message;
            if let Some(base) = message.export_key.take() {
                let n = seen
                    .entry((session.pk, message.mark.clone(), base.clone()))
                    .or_insert(0);
                message.export_key = Some(if *n == 0 { base } else { format!("{base}-{n}") });
                *n += 1;
            }
            chat.messages.push(message);
        }

        let mut writer = ChatWriter {
            space,
            by: by
                .clone()
                .with_source(self.database().to_string_lossy().into_owned()),
            report: ChatImportReport::new(ChatFormat::WhatsApp),
        };
        for chat in chats.values_mut() {
            chat.collect_participants();
            writer.write(chat, &mut files)?;
        }
        Ok(SyncReport {
            import: writer.report,
            read,
            cursor: next,
        })
    }

    /// A private copy of the database, checked.
    fn copy(&self) -> Result<DbCopy, WhatsAppMacError> {
        let database = self.database();
        let denied = || WhatsAppMacError::NoFullDiskAccess(database.clone());
        match fs::metadata(&self.container) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(WhatsAppMacError::NotInstalled(database));
            }
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return Err(denied()),
            Err(e) => return Err(e.into()),
        }
        // Opening the file tells a missing database from one this process may not read, which
        // SQLite would report alike.
        match fs::File::open(&database) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(WhatsAppMacError::NotInstalled(database));
            }
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return Err(denied()),
            Err(e) => return Err(e.into()),
        }
        let source = Connection::open_with_flags(
            &database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| match e.sqlite_error_code() {
            Some(
                rusqlite::ErrorCode::PermissionDenied
                | rusqlite::ErrorCode::AuthorizationForStatementDenied,
            ) => denied(),
            _ => WhatsAppMacError::Database(e),
        })?;
        source.busy_timeout(Duration::from_secs(5))?;
        source.execute_batch("PRAGMA query_only = ON")?;

        let dir = TempDir::new()?;
        let mut connection = Connection::open(dir.path().join(DATABASE))?;
        {
            let backup = Backup::new(&source, &mut connection)?;
            let mut waits = 0;
            loop {
                match backup.step(-1)? {
                    StepResult::Done => break,
                    StepResult::More => {}
                    _ if waits < 50 => {
                        waits += 1;
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    _ => {
                        return Err(WhatsAppMacError::Database(rusqlite::Error::SqliteFailure(
                            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                            Some("WhatsApp kept the database busy".into()),
                        )));
                    }
                }
            }
        }
        drop(source);
        connection.execute_batch("PRAGMA query_only = ON")?;
        check_schema(&connection)?;
        Ok(DbCopy { connection, dir })
    }
}

/// A copy of the database, deleted with its folder when dropped.
struct DbCopy {
    // Closed before the folder is removed: fields drop in order.
    connection: Connection,
    dir: TempDir,
}

/// A folder only this user may open, removed with what is in it when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> io::Result<TempDir> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "knode-whatsapp-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path)?;
        Ok(TempDir(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
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
) -> Result<Vec<Stored>, WhatsAppMacError> {
    // The chat keys are integers from the database itself.
    let list = sessions
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
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
         ORDER BY m.ZMESSAGEDATE, m.Z_PK"
    );
    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement.query(rusqlite::params![after, since.unwrap_or(f64::MIN)])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(Stored {
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
    fn removes_its_copy() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        fs::write(path.join(DATABASE), b"copy").unwrap();
        drop(dir);
        assert!(!path.exists());
    }

    #[test]
    fn tells_numbers_from_names() {
        assert!(is_number("+34 600 000 000"));
        assert!(!is_number("Ada"));
    }
}
