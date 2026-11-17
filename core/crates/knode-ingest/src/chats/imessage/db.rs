//! Reading the Messages database from a copy ([`sqlite::open_copy`]): the database and its
//! write-ahead log are copied as files into a private temporary folder and only the copy is
//! opened, so SQLite never opens the database Messages uses and nothing of it is locked,
//! changed or written. The copy is checked for every table and column read before anything is
//! read.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::{OptionalExtension, params};

use crate::sources::SourceError;
use crate::sources::sqlite::{self, DatabaseCopy};

/// The tables and columns read, which the database must have.
pub(crate) const REQUIRED: &[(&str, &[&str])] = &[
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

/// A checked copy of the Messages database, removed when dropped.
pub(crate) struct Snapshot {
    copy: DatabaseCopy,
    /// The optional columns of `message` the database has.
    optional: Vec<&'static str>,
}

/// The size and time of change of the database and its log, which change whenever Messages
/// writes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp(Vec<(u64, Option<SystemTime>)>);

pub(crate) fn wal_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push("-wal");
    PathBuf::from(name)
}

fn stamp(db: &Path) -> Stamp {
    Stamp(
        [db.to_path_buf(), wal_path(db)]
            .iter()
            .map(|path| match fs::metadata(path) {
                Ok(meta) => (meta.len(), meta.modified().ok()),
                Err(_) => (0, None),
            })
            .collect(),
    )
}

impl Snapshot {
    /// Copies the database at `db` (and its log) and opens the copy, after checking it has
    /// every table and column read: [`SourceError::NeedsUpdate`] when one is missing.
    pub(crate) fn take(db: &Path) -> Result<Snapshot, SourceError> {
        let mut attempt = 1;
        let copy = loop {
            let before = stamp(db);
            let copy = sqlite::open_copy(db)?;
            // Copied while Messages wrote: the database and its log may not match.
            if stamp(db) == before || attempt == COPY_ATTEMPTS {
                break copy;
            }
            attempt += 1;
        };
        sqlite::check_schema(copy.conn(), REQUIRED)?;
        let columns = table_columns(&copy, "message").map_err(failed)?;
        let optional = OPTIONAL
            .iter()
            .copied()
            .filter(|c| columns.iter().any(|h| h == c))
            .collect();
        Ok(Snapshot { copy, optional })
    }

    fn conn(&self) -> &rusqlite::Connection {
        self.copy.conn()
    }

    fn has(&self, column: &str) -> bool {
        self.optional.contains(&column)
    }

    /// Every conversation.
    pub(crate) fn chats(&self) -> rusqlite::Result<Vec<ChatRow>> {
        let mut stmt = self.conn().prepare(
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
        let mut stmt = self.conn().prepare(
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
    pub(crate) fn last_rowid(&self) -> rusqlite::Result<i64> {
        Ok(self
            .conn()
            .query_row("SELECT MAX(ROWID) FROM message", [], |r| {
                r.get::<_, Option<i64>>(0)
            })?
            .unwrap_or(0))
    }

    /// The messages of the conversations `chats` after message number `after` and, with
    /// `from`, at or after that Apple time: at most `limit`, in the order they were stored, each
    /// with the number of its conversation.
    pub(crate) fn messages(
        &self,
        chats: &[i64],
        after: i64,
        from: Option<i64>,
        limit: usize,
    ) -> rusqlite::Result<Vec<(i64, MessageRow)>> {
        if chats.is_empty() {
            return Ok(Vec::new());
        }
        let col = |name: &str, default: &str| {
            if self.has(name) {
                format!("m.{name}")
            } else {
                default.to_string()
            }
        };
        let ids: Vec<String> = chats.iter().map(i64::to_string).collect();
        let sql = format!(
            "SELECT j.chat_id, m.ROWID, m.guid, m.text, m.attributedBody, h.id, m.date, \
             m.is_from_me, m.associated_message_type, m.item_type, {}, {}, {}, {} \
             FROM chat_message_join j JOIN message m ON m.ROWID = j.message_id \
             LEFT JOIN handle h ON h.ROWID = m.handle_id \
             WHERE j.chat_id IN ({}) AND m.ROWID > ?1 AND (?2 IS NULL OR m.date >= ?2) \
             ORDER BY m.ROWID, j.chat_id LIMIT ?3",
            col("date_edited", "0"),
            col("date_retracted", "0"),
            col("group_title", "NULL"),
            col("thread_originator_guid", "NULL"),
            ids.join(", "),
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt.query_map(params![after, from, limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                MessageRow {
                    rowid: r.get(1)?,
                    guid: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    text: r.get(3)?,
                    body: r.get(4)?,
                    handle: r.get(5)?,
                    date: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                    from_me: r.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0,
                    associated_type: r.get::<_, Option<i64>>(8)?.unwrap_or(0),
                    item_type: r.get::<_, Option<i64>>(9)?.unwrap_or(0),
                    date_edited: r.get::<_, Option<i64>>(10)?.unwrap_or(0),
                    date_retracted: r.get::<_, Option<i64>>(11)?.unwrap_or(0),
                    group_title: r.get(12)?,
                    reply_to: r.get(13)?,
                    attachments: Vec::new(),
                },
            ))
        })?;
        let mut messages: Vec<(i64, MessageRow)> = rows.collect::<Result<_, _>>()?;
        let mut stmt = self.conn().prepare(
            "SELECT a.filename, a.transfer_name, a.mime_type FROM message_attachment_join j \
             JOIN attachment a ON a.ROWID = j.attachment_id WHERE j.message_id = ?1 \
             ORDER BY a.ROWID",
        )?;
        for (_, message) in &mut messages {
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
    pub(crate) fn message_by_guid(&self, guid: &str) -> rusqlite::Result<Option<MessageRow>> {
        let row = self
            .conn()
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

fn table_columns(copy: &DatabaseCopy, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = copy
        .conn()
        .prepare("SELECT name FROM pragma_table_info(?1)")?;
    let rows = stmt.query_map([table], |r| r.get(0))?;
    rows.collect()
}

/// A query on the copy failed.
pub(crate) fn failed(error: rusqlite::Error) -> SourceError {
    SourceError::Failed(format!(
        "the copy of the Messages database could not be read: {error}"
    ))
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
