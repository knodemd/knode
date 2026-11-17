//! Reading the Messages database from a copy: the database and its write-ahead log are copied
//! as files, the copy is opened and backed up with SQLite's backup API into a snapshot that
//! holds every committed message, and only the snapshot is read. SQLite never opens the
//! database Messages uses, so nothing of it is locked, changed or written.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use super::IMessageError;

/// The tables and columns read, which the database must have.
const REQUIRED: &[(&str, &[&str])] = &[
    (
        "message",
        &[
            "guid",
            "text",
            "attributedBody",
            "handle_id",
            "date",
            "is_from_me",
            "associated_message_type",
            "item_type",
        ],
    ),
    (
        "chat",
        &["guid", "style", "chat_identifier", "display_name"],
    ),
    ("handle", &["id"]),
    ("chat_message_join", &["chat_id", "message_id"]),
    ("chat_handle_join", &["chat_id", "handle_id"]),
    ("attachment", &["filename", "transfer_name", "mime_type"]),
    ("message_attachment_join", &["message_id", "attachment_id"]),
];

/// Columns read when the database has them; older versions of Messages do not.
const OPTIONAL: &[&str] = &[
    "date_edited",
    "date_retracted",
    "group_title",
    "thread_originator_guid",
];

/// How many times the files are copied again when Messages wrote to them during the copy.
const COPY_ATTEMPTS: usize = 3;

/// A snapshot of the Messages database in a private temporary folder, removed when dropped.
pub(crate) struct Snapshot {
    conn: Connection,
    dir: PathBuf,
    /// The optional columns of `message` the database has.
    optional: Vec<&'static str>,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// The size and time of change of the database and its log, which change whenever Messages
/// writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp(Vec<(u64, Option<SystemTime>)>);

pub(crate) fn wal_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push("-wal");
    PathBuf::from(name)
}

/// The stamp of the database at `db`.
pub(crate) fn stamp(db: &Path) -> Result<Stamp, IMessageError> {
    let mut parts = Vec::new();
    for (i, path) in [db.to_path_buf(), wal_path(db)].iter().enumerate() {
        match fs::metadata(path) {
            Ok(meta) => parts.push((meta.len(), meta.modified().ok())),
            Err(e) if i > 0 && e.kind() == io::ErrorKind::NotFound => parts.push((0, None)),
            Err(e) => return Err(IMessageError::from_open(db, e)),
        }
    }
    Ok(Stamp(parts))
}

/// A private folder for the copy.
fn private_dir() -> io::Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "knode-imessage-{}-{nanos}",
        std::process::id()
    ));
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&dir)?;
    Ok(dir)
}

impl Snapshot {
    /// Copies the database at `db` (and its log) and opens the copy, after checking it has
    /// every table and column read.
    pub(crate) fn take(db: &Path) -> Result<Snapshot, IMessageError> {
        // Fails here, before anything is copied, without Full Disk Access.
        fs::File::open(db).map_err(|e| IMessageError::from_open(db, e))?;
        let dir = private_dir()?;
        let result = Self::take_into(db, &dir);
        if result.is_err() {
            let _ = fs::remove_dir_all(&dir);
        }
        result
    }

    fn take_into(db: &Path, dir: &Path) -> Result<Snapshot, IMessageError> {
        let raw = dir.join("copy.db");
        for attempt in 1..=COPY_ATTEMPTS {
            let before = stamp(db)?;
            fs::copy(db, &raw).map_err(|e| IMessageError::from_open(db, e))?;
            let wal = wal_path(db);
            let raw_wal = wal_path(&raw);
            match fs::copy(&wal, &raw_wal) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    let _ = fs::remove_file(&raw_wal);
                }
                Err(e) => return Err(IMessageError::from_open(db, e)),
            }
            if stamp(db)? == before || attempt == COPY_ATTEMPTS {
                break;
            }
        }
        // The copy is opened (which applies its log), then backed up into the snapshot read.
        let snapshot = dir.join("snapshot.db");
        {
            let copy = Connection::open_with_flags(
                &raw,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            copy.backup(rusqlite::MAIN_DB, &snapshot, None)?;
        }
        for path in [raw.clone(), wal_path(&raw), dir.join("copy.db-shm")] {
            let _ = fs::remove_file(path);
        }
        let conn = Connection::open_with_flags(
            &snapshot,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let optional = check_schema(&conn)?;
        Ok(Snapshot {
            conn,
            dir: dir.to_path_buf(),
            optional,
        })
    }

    fn has(&self, column: &str) -> bool {
        self.optional.contains(&column)
    }

    /// Every conversation.
    pub(crate) fn chats(&self) -> Result<Vec<ChatRow>, IMessageError> {
        let mut stmt = self.conn.prepare(
            "SELECT ROWID, guid, style, chat_identifier, display_name FROM chat ORDER BY ROWID",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ChatRow {
                rowid: r.get(0)?,
                guid: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                style: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                identifier: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                display_name: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                handles: Vec::new(),
            })
        })?;
        let mut chats: Vec<ChatRow> = rows.collect::<Result<_, _>>()?;
        let mut stmt = self.conn.prepare(
            "SELECT j.chat_id, h.id FROM chat_handle_join j JOIN handle h ON h.ROWID = j.handle_id \
             ORDER BY j.chat_id, h.ROWID",
        )?;
        let mut handles: BTreeMap<i64, Vec<String>> = BTreeMap::new();
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (chat, handle) = row?;
            let list = handles.entry(chat).or_default();
            if !list.contains(&handle) {
                list.push(handle);
            }
        }
        for chat in &mut chats {
            chat.handles = handles.remove(&chat.rowid).unwrap_or_default();
        }
        Ok(chats)
    }

    /// The highest message number, or 0 with no messages.
    pub(crate) fn last_rowid(&self) -> Result<i64, IMessageError> {
        Ok(self
            .conn
            .query_row("SELECT MAX(ROWID) FROM message", [], |r| {
                r.get::<_, Option<i64>>(0)
            })?
            .unwrap_or(0))
    }

    /// The messages of a conversation after message number `after` and at or after Apple time
    /// `from`, in the order they were stored.
    pub(crate) fn messages(
        &self,
        chat: i64,
        after: i64,
        from: Option<i64>,
    ) -> Result<Vec<MessageRow>, IMessageError> {
        let col = |name: &str, default: &str| {
            if self.has(name) {
                format!("m.{name}")
            } else {
                default.to_string()
            }
        };
        let sql = format!(
            "SELECT m.ROWID, m.guid, m.text, m.attributedBody, h.id, m.date, m.is_from_me, \
             m.associated_message_type, m.item_type, {}, {}, {}, {} \
             FROM chat_message_join j JOIN message m ON m.ROWID = j.message_id \
             LEFT JOIN handle h ON h.ROWID = m.handle_id \
             WHERE j.chat_id = ?1 AND m.ROWID > ?2 AND (?3 IS NULL OR m.date >= ?3) \
             ORDER BY m.ROWID",
            col("date_edited", "0"),
            col("date_retracted", "0"),
            col("group_title", "NULL"),
            col("thread_originator_guid", "NULL"),
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![chat, after, from], |r| {
            Ok(MessageRow {
                rowid: r.get(0)?,
                guid: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                text: r.get(2)?,
                body: r.get(3)?,
                handle: r.get(4)?,
                date: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                from_me: r.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
                associated_type: r.get::<_, Option<i64>>(7)?.unwrap_or(0),
                item_type: r.get::<_, Option<i64>>(8)?.unwrap_or(0),
                date_edited: r.get::<_, Option<i64>>(9)?.unwrap_or(0),
                date_retracted: r.get::<_, Option<i64>>(10)?.unwrap_or(0),
                group_title: r.get(11)?,
                reply_to: r.get(12)?,
                attachments: Vec::new(),
            })
        })?;
        let mut messages: Vec<MessageRow> = rows.collect::<Result<_, _>>()?;
        let mut stmt = self.conn.prepare(
            "SELECT a.filename, a.transfer_name, a.mime_type FROM message_attachment_join j \
             JOIN attachment a ON a.ROWID = j.attachment_id WHERE j.message_id = ?1 \
             ORDER BY a.ROWID",
        )?;
        for message in &mut messages {
            let rows = stmt.query_map([message.rowid], |r| {
                Ok(AttachmentRow {
                    filename: r.get(0)?,
                    transfer_name: r.get(1)?,
                    mime_type: r.get(2)?,
                })
            })?;
            message.attachments = rows.collect::<Result<_, _>>()?;
        }
        Ok(messages)
    }

    /// The message with this guid, if any: who sent it, when, and its text.
    pub(crate) fn message_by_guid(&self, guid: &str) -> Result<Option<MessageRow>, IMessageError> {
        let row = self
            .conn
            .query_row(
                "SELECT m.ROWID, m.text, m.attributedBody, h.id, m.date, m.is_from_me \
                 FROM message m LEFT JOIN handle h ON h.ROWID = m.handle_id WHERE m.guid = ?1",
                [guid],
                |r| {
                    Ok(MessageRow {
                        rowid: r.get(0)?,
                        guid: guid.to_string(),
                        text: r.get(1)?,
                        body: r.get(2)?,
                        handle: r.get(3)?,
                        date: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        from_me: r.get::<_, Option<i64>>(5)?.unwrap_or(0) != 0,
                        associated_type: 0,
                        item_type: 0,
                        date_edited: 0,
                        date_retracted: 0,
                        group_title: None,
                        reply_to: None,
                        attachments: Vec::new(),
                    })
                },
            )
            .optional()?;
        Ok(row)
    }
}

/// The optional columns the database has, once every required one is found; otherwise the
/// connector needs an update.
fn check_schema(conn: &Connection) -> Result<Vec<&'static str>, IMessageError> {
    let mut missing = Vec::new();
    let mut message_columns = Vec::new();
    for (table, columns) in REQUIRED {
        let mut stmt = conn.prepare("SELECT name FROM pragma_table_info(?1)")?;
        let have: Vec<String> = stmt
            .query_map([table], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        if have.is_empty() {
            missing.push(format!("table {table}"));
            continue;
        }
        for column in *columns {
            if !have.iter().any(|h| h == column) {
                missing.push(format!("{table}.{column}"));
            }
        }
        if *table == "message" {
            message_columns = have;
        }
    }
    if !missing.is_empty() {
        return Err(IMessageError::NeedsUpdate { missing });
    }
    Ok(OPTIONAL
        .iter()
        .copied()
        .filter(|c| message_columns.iter().any(|h| h == c))
        .collect())
}

/// A conversation.
#[derive(Debug, Clone)]
pub(crate) struct ChatRow {
    pub rowid: i64,
    pub guid: String,
    /// 43 for a group, 45 for one person.
    pub style: i64,
    pub identifier: String,
    pub display_name: String,
    /// The phone numbers and email addresses of the others in it.
    pub handles: Vec<String>,
}

/// A message.
#[derive(Debug, Clone)]
pub(crate) struct MessageRow {
    pub rowid: i64,
    pub guid: String,
    pub text: Option<String>,
    /// `attributedBody`: the text, archived.
    pub body: Option<Vec<u8>>,
    /// The phone number or email address of who sent it, when it was not the person.
    pub handle: Option<String>,
    /// Apple time: since 2001-01-01, in nanoseconds (in seconds before macOS 10.13).
    pub date: i64,
    pub from_me: bool,
    /// 2000-2999 a reaction, 3000-3999 a reaction taken back.
    pub associated_type: i64,
    /// 0 a message; others are events, such as 2 for a new name for the group.
    pub item_type: i64,
    pub date_edited: i64,
    pub date_retracted: i64,
    pub group_title: Option<String>,
    /// `thread_originator_guid`: the message this one replies to.
    pub reply_to: Option<String>,
    pub attachments: Vec<AttachmentRow>,
}

/// A file sent with a message.
#[derive(Debug, Clone)]
pub(crate) struct AttachmentRow {
    /// Where Messages keeps it, often starting with `~/Library/Messages/`.
    pub filename: Option<String>,
    /// Its name as sent.
    pub transfer_name: Option<String>,
    pub mime_type: Option<String>,
}
