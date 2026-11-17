//! The iMessage connector, against synthetic Messages databases built here with the tables and
//! columns of the real one: invented conversations between invented people.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset, Utc};
use knode_format::{Node, Provenance, Space};
use knode_ingest::imessage::{
    self, ContactNames, Cursor, History, IMessageError, MessagesSource, SyncOptions, SyncReport,
};
use knode_ingest::{CHAT_KIND, chat_keys};
use rusqlite::{Connection, params};
use tempfile::TempDir;

/// The tables of the Messages database, with the columns read and some of the others.
const SCHEMA: &str = "
CREATE TABLE message (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, guid TEXT UNIQUE NOT NULL,
  text TEXT, replace INTEGER DEFAULT 0, service_center TEXT, handle_id INTEGER DEFAULT 0,
  subject TEXT, country TEXT, attributedBody BLOB, version INTEGER DEFAULT 0,
  type INTEGER DEFAULT 0, service TEXT, account TEXT, date INTEGER, date_read INTEGER,
  date_delivered INTEGER, is_delivered INTEGER DEFAULT 0, is_finished INTEGER DEFAULT 0,
  is_from_me INTEGER DEFAULT 0, is_read INTEGER DEFAULT 0, cache_has_attachments INTEGER DEFAULT 0,
  item_type INTEGER DEFAULT 0, other_handle INTEGER DEFAULT 0, group_title TEXT,
  group_action_type INTEGER DEFAULT 0, associated_message_guid TEXT,
  associated_message_type INTEGER DEFAULT 0, balloon_bundle_id TEXT, payload_data BLOB,
  thread_originator_guid TEXT, date_retracted INTEGER DEFAULT 0, date_edited INTEGER DEFAULT 0,
  message_summary_info BLOB);
CREATE TABLE chat (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, guid TEXT UNIQUE NOT NULL,
  style INTEGER, state INTEGER, account_id TEXT, properties BLOB, chat_identifier TEXT,
  service_name TEXT, room_name TEXT, account_login TEXT, is_archived INTEGER DEFAULT 0,
  last_addressed_handle TEXT, display_name TEXT, group_id TEXT);
CREATE TABLE handle (ROWID INTEGER PRIMARY KEY AUTOINCREMENT UNIQUE, id TEXT NOT NULL,
  country TEXT, service TEXT NOT NULL, uncanonicalized_id TEXT, person_centric_id TEXT,
  UNIQUE (id, service));
CREATE TABLE chat_message_join (chat_id INTEGER REFERENCES chat (ROWID) ON DELETE CASCADE,
  message_id INTEGER REFERENCES message (ROWID) ON DELETE CASCADE,
  message_date INTEGER DEFAULT 0, PRIMARY KEY (chat_id, message_id));
CREATE TABLE chat_handle_join (chat_id INTEGER REFERENCES chat (ROWID) ON DELETE CASCADE,
  handle_id INTEGER REFERENCES handle (ROWID) ON DELETE CASCADE, UNIQUE(chat_id, handle_id));
CREATE TABLE attachment (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, guid TEXT UNIQUE NOT NULL,
  created_date INTEGER DEFAULT 0, start_date INTEGER DEFAULT 0, filename TEXT, uti TEXT,
  mime_type TEXT, transfer_state INTEGER DEFAULT 0, is_outgoing INTEGER DEFAULT 0,
  transfer_name TEXT, total_bytes INTEGER DEFAULT 0);
CREATE TABLE message_attachment_join (message_id INTEGER REFERENCES message (ROWID) ON DELETE CASCADE,
  attachment_id INTEGER REFERENCES attachment (ROWID) ON DELETE CASCADE,
  UNIQUE(message_id, attachment_id));
";

const ADA: &str = "+15550100";
const GRACE: &str = "grace@example.com";
const LINUS: &str = "+15550111";
const ADA_CHAT: &str = "iMessage;-;+15550100";
const TRIP_CHAT: &str = "iMessage;+;chat000000000000000001";

/// An `NSAttributedString` archived as Messages writes `attributedBody`.
fn attributed_body(text: &str) -> Vec<u8> {
    let mut out = vec![0x04, 0x0B];
    out.extend_from_slice(b"streamtyped");
    out.extend_from_slice(&[0x81, 0xE8, 0x03, 0x84, 0x01, 0x40, 0x84, 0x84, 0x84, 0x12]);
    out.extend_from_slice(b"NSAttributedString");
    out.extend_from_slice(&[0x00, 0x84, 0x84, 0x08]);
    out.extend_from_slice(b"NSObject");
    out.extend_from_slice(&[0x00, 0x85, 0x92, 0x84, 0x84, 0x84, 0x08]);
    out.extend_from_slice(b"NSString");
    out.extend_from_slice(&[0x01, 0x94, 0x84, 0x01, 0x2B]);
    let len = text.len();
    if len < 0x80 {
        out.push(len as u8);
    } else {
        out.push(0x81);
        out.extend_from_slice(&(len as u16).to_le_bytes());
    }
    out.extend_from_slice(text.as_bytes());
    out.push(0x86);
    // A run of attributes: index and length, then a dictionary, as Messages writes after the
    // string.
    out.extend_from_slice(&[0x84, 0x02, b'i', b'I', 0x01, len.min(0x7F) as u8, 0x92]);
    out.extend_from_slice(&[0x84, 0x84, 0x84, 0x0C]);
    out.extend_from_slice(b"NSDictionary");
    out.extend_from_slice(&[0x00, 0x94, 0x84, 0x01, b'i', 0x01, 0x86, 0x86]);
    out
}

/// Apple time, in nanoseconds, of a UTC time `YYYY-MM-DDTHH:MM`.
fn at(time: &str) -> i64 {
    let t = DateTime::parse_from_rfc3339(&format!("{time}:00Z")).unwrap();
    (t.timestamp() - 978_307_200) * 1_000_000_000
}

/// A synthetic Messages folder: `chat.db` (in WAL mode, kept open so what is written stays in
/// the log) and `Attachments`.
struct Messages {
    dir: TempDir,
    conn: Connection,
}

impl Messages {
    fn new() -> Messages {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open(dir.path().join("chat.db")).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        let messages = Messages { dir, conn };
        for (id, service) in [(ADA, "iMessage"), (GRACE, "iMessage"), (LINUS, "SMS")] {
            messages
                .conn
                .execute(
                    "INSERT INTO handle (id, service) VALUES (?1, ?2)",
                    params![id, service],
                )
                .unwrap();
        }
        messages
    }

    fn source(&self) -> MessagesSource {
        MessagesSource::new(self.dir.path())
    }

    fn handle(&self, id: &str) -> i64 {
        self.conn
            .query_row("SELECT ROWID FROM handle WHERE id = ?1", [id], |r| r.get(0))
            .unwrap()
    }

    fn chat(&self, guid: &str, style: i64, name: &str, handles: &[&str]) -> i64 {
        let identifier = guid.rsplit(';').next().unwrap();
        self.conn
            .execute(
                "INSERT INTO chat (guid, style, chat_identifier, service_name, display_name) \
                 VALUES (?1, ?2, ?3, 'iMessage', ?4)",
                params![guid, style, identifier, name],
            )
            .unwrap();
        let chat = self.conn.last_insert_rowid();
        for h in handles {
            self.conn
                .execute(
                    "INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (?1, ?2)",
                    params![chat, self.handle(h)],
                )
                .unwrap();
        }
        chat
    }

    fn chat_id(&self, guid: &str) -> i64 {
        self.conn
            .query_row("SELECT ROWID FROM chat WHERE guid = ?1", [guid], |r| {
                r.get(0)
            })
            .unwrap()
    }

    /// A message from `from` (`None` for the person) in `chat`; returns its guid.
    fn message(&self, chat: &str, from: Option<&str>, time: &str, text: &str) -> String {
        self.insert(chat, from, time, Some(text), None, &[])
    }

    fn insert(
        &self,
        chat: &str,
        from: Option<&str>,
        time: &str,
        text: Option<&str>,
        body: Option<Vec<u8>>,
        extra: &[(&str, rusqlite::types::Value)],
    ) -> String {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM message", [], |r| r.get(0))
            .unwrap();
        let guid = format!("00000000-0000-4000-8000-{:012}", n + 1);
        let handle = from.map_or(0, |h| self.handle(h));
        self.conn
            .execute(
                "INSERT INTO message (guid, text, attributedBody, handle_id, date, is_from_me, \
                 service) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'iMessage')",
                params![guid, text, body, handle, at(time), from.is_none()],
            )
            .unwrap();
        let rowid = self.conn.last_insert_rowid();
        for (column, value) in extra {
            self.conn
                .execute(
                    &format!("UPDATE message SET {column} = ?1 WHERE ROWID = ?2"),
                    params![value, rowid],
                )
                .unwrap();
        }
        self.conn
            .execute(
                "INSERT INTO chat_message_join (chat_id, message_id, message_date) \
                 VALUES (?1, ?2, ?3)",
                params![self.chat_id(chat), rowid, at(time)],
            )
            .unwrap();
        guid
    }

    /// Adds a file to the last message, written under `Attachments` when `bytes` is given.
    fn attach(&self, name: &str, mime: &str, bytes: Option<&[u8]>) {
        let message: i64 = self
            .conn
            .query_row("SELECT MAX(ROWID) FROM message", [], |r| r.get(0))
            .unwrap();
        let rel = format!("Attachments/0{message}/00/AAAA-{message}/{name}");
        if let Some(bytes) = bytes {
            let path = self.dir.path().join(&rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        self.conn
            .execute(
                "INSERT INTO attachment (guid, filename, mime_type, transfer_name) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    format!("at_{message}_{name}"),
                    format!("~/Library/Messages/{rel}"),
                    mime,
                    name
                ],
            )
            .unwrap();
        let attachment = self.conn.last_insert_rowid();
        self.conn
            .execute(
                "UPDATE message SET cache_has_attachments = 1 WHERE ROWID = ?1",
                [message],
            )
            .unwrap();
        self.conn
            .execute(
                "INSERT INTO message_attachment_join (message_id, attachment_id) VALUES (?1, ?2)",
                params![message, attachment],
            )
            .unwrap();
    }
}

fn space(dir: &TempDir) -> Space {
    Space::create(dir.path().join("Test.knode"), "Test").unwrap()
}

fn by() -> Provenance {
    Provenance::person("example")
}

fn options() -> SyncOptions<'static> {
    SyncOptions {
        history: History::Everything,
        offset: FixedOffset::east_opt(0),
        ..Default::default()
    }
}

fn chat_node(space: &Space, guid: &str) -> Node {
    let found = space.find_by_property(chat_keys::ID, guid).unwrap();
    assert_eq!(found.len(), 1, "one chat {guid}");
    space.get_node(&found[0].id).unwrap().unwrap()
}

fn chat_count(space: &Space) -> usize {
    space
        .find_by_property(chat_keys::APP, imessage::APP)
        .unwrap()
        .len()
}

fn prop(space: &Space, node: &Node, key: &str) -> String {
    space.property(&node.id, key).unwrap().unwrap_or_default()
}

fn sync(
    space: &mut Space,
    messages: &Messages,
    cursor: Option<Cursor>,
    options: &SyncOptions<'_>,
) -> SyncReport {
    imessage::sync(space, &messages.source(), cursor, options, &by()).unwrap()
}

/// Two conversations: one with Ada, and a group named "Trip".
fn two_conversations() -> Messages {
    let m = Messages::new();
    m.chat(ADA_CHAT, 45, "", &[ADA]);
    m.chat(TRIP_CHAT, 43, "Trip", &[GRACE, LINUS]);
    m
}

#[test]
fn brings_each_conversation_into_a_chat_node() {
    let m = two_conversations();
    let question = m.message(ADA_CHAT, Some(ADA), "2026-03-04T09:15", "Dinner on Friday?");
    m.insert(
        ADA_CHAT,
        None,
        "2026-03-04T09:16",
        None,
        Some(attributed_body("Yes! Eight at the usual place")),
        &[],
    );
    // A reaction to the question, and one taken back: left out.
    m.insert(
        ADA_CHAT,
        Some(ADA),
        "2026-03-04T09:17",
        Some("Loved \u{201c}Yes! Eight at the usual place\u{201d}"),
        None,
        &[
            ("associated_message_type", 2000.into()),
            ("associated_message_guid", format!("p:0/{question}").into()),
        ],
    );
    m.insert(
        ADA_CHAT,
        Some(ADA),
        "2026-03-04T09:18",
        Some("Removed a heart"),
        None,
        &[("associated_message_type", 3000.into())],
    );
    m.insert(
        ADA_CHAT,
        Some(ADA),
        "2026-03-05T08:00",
        Some("# Bring the map\n[[secret]]"),
        None,
        &[("date_edited", at("2026-03-05T08:01").into())],
    );
    m.insert(
        ADA_CHAT,
        Some(ADA),
        "2026-03-05T08:02",
        None,
        None,
        &[("date_retracted", at("2026-03-05T08:03").into())],
    );
    m.insert(
        ADA_CHAT,
        None,
        "2026-03-05T08:05",
        Some("Sure"),
        None,
        &[("thread_originator_guid", question.clone().into())],
    );

    m.message(
        TRIP_CHAT,
        Some(GRACE),
        "2026-03-04T10:00",
        "Tickets are booked",
    );
    m.insert(
        TRIP_CHAT,
        Some(LINUS),
        "2026-03-04T10:01",
        Some("\u{FFFC}"),
        None,
        &[],
    );
    m.attach("IMG_0001.HEIC", "image/heic", Some(b"not really a photo"));
    m.insert(TRIP_CHAT, Some(LINUS), "2026-03-04T10:02", None, None, &[]);
    m.attach("Itinerary.pdf", "application/pdf", None);
    m.insert(
        TRIP_CHAT,
        Some(GRACE),
        "2026-03-04T10:03",
        None,
        None,
        &[
            ("item_type", 2.into()),
            ("group_title", String::from("Trip to the coast").into()),
        ],
    );
    m.insert(
        TRIP_CHAT,
        Some(GRACE),
        "2026-03-04T10:04",
        None,
        None,
        &[("item_type", 1.into())],
    );

    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let report = sync(&mut space, &m, None, &options());
    assert_eq!(report.chats.chats.len(), 2);
    assert_eq!(report.chats.messages, 9);
    assert_eq!(report.reactions, 2);
    assert_eq!(report.skipped_events, 1);
    assert_eq!(report.chats.attached, 1);
    assert_eq!(report.chats.missing, 1);
    assert_eq!(report.cursor, Cursor(12));

    let ada = chat_node(&space, ADA_CHAT);
    assert_eq!(ada.kind, CHAT_KIND);
    assert_eq!(ada.title, "iMessage - +15550100");
    assert_eq!(
        ada.body,
        "## 2026-03-04\n\n\
         **+15550100** (09:15): Dinner on Friday?\n\n\
         **Me** (09:16): Yes! Eight at the usual place\n\n\
         ## 2026-03-05\n\n\
         **+15550100** (08:00): \\# Bring the map\n\\[\\[secret\\]\\]\n*(edited)*\n\n\
         **+15550100** (08:02): *[unsent]*\n\n\
         **Me** (08:05, replying to +15550100 at 09:15: Dinner on Friday?): Sure\n"
    );
    assert_eq!(prop(&space, &ada, chat_keys::APP), "imessage");
    assert_eq!(prop(&space, &ada, chat_keys::TYPE), "personal");
    assert_eq!(prop(&space, &ada, chat_keys::PARTICIPANTS), "+15550100, Me");
    assert_eq!(
        prop(&space, &ada, chat_keys::STARTED_AT),
        "2026-03-04T09:15:00"
    );
    assert_eq!(
        prop(&space, &ada, chat_keys::ENDED_AT),
        "2026-03-05T08:05:00"
    );
    assert_eq!(prop(&space, &ada, chat_keys::MESSAGES), "5");

    let trip = chat_node(&space, TRIP_CHAT);
    assert_eq!(trip.title, "iMessage - Trip");
    assert_eq!(
        trip.body,
        "## 2026-03-04\n\n\
         **grace@example.com** (10:00): Tickets are booked\n\n\
         **+15550111** (10:01): ![[IMG_0001.HEIC]]\n\n\
         **+15550111** (10:02): *[not attached: Itinerary.pdf, not on this Mac]*\n\n\
         *(10:03) grace@example.com named the conversation \u{201c}Trip to the coast\u{201d}*\n"
    );
    assert_eq!(prop(&space, &trip, chat_keys::TYPE), "group");
    assert_eq!(
        prop(&space, &trip, chat_keys::PARTICIPANTS),
        "grace@example.com, +15550111, Me"
    );
    let files = space.attachments(&trip.id).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].name, "IMG_0001.HEIC");
}

#[test]
fn reads_what_is_still_only_in_the_write_ahead_log() {
    let m = two_conversations();
    m.message(ADA_CHAT, Some(ADA), "2026-03-04T09:15", "Only in the log");
    let wal = m.dir.path().join("chat.db-wal");
    assert!(
        fs::metadata(&wal).unwrap().len() > 0,
        "the test keeps a log"
    );

    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let report = sync(&mut space, &m, None, &options());
    assert_eq!(report.chats.messages, 1);
    assert!(chat_node(&space, ADA_CHAT).body.contains("Only in the log"));
}

#[test]
fn never_changes_the_messages_database() {
    let m = two_conversations();
    m.message(ADA_CHAT, Some(ADA), "2026-03-04T09:15", "Hello");
    let files: Vec<PathBuf> = ["chat.db", "chat.db-wal", "chat.db-shm"]
        .iter()
        .map(|n| m.dir.path().join(n))
        .collect();
    let before: Vec<_> = files
        .iter()
        .map(|f| {
            (
                fs::read(f).unwrap(),
                fs::metadata(f).unwrap().modified().unwrap(),
            )
        })
        .collect();
    let listing = |dir: &Path| {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let names = listing(m.dir.path());

    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    sync(&mut space, &m, None, &options());
    imessage::conversations(&m.source(), None).unwrap();

    let after: Vec<_> = files
        .iter()
        .map(|f| {
            (
                fs::read(f).unwrap(),
                fs::metadata(f).unwrap().modified().unwrap(),
            )
        })
        .collect();
    assert!(
        before == after,
        "the database, its log and its index are untouched"
    );
    assert_eq!(listing(m.dir.path()), names, "no file added next to it");
}

#[test]
fn syncing_again_adds_only_new_messages() {
    let m = two_conversations();
    m.message(ADA_CHAT, Some(ADA), "2026-03-04T09:15", "First");
    m.message(TRIP_CHAT, Some(GRACE), "2026-03-04T09:20", "Second");
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let first = sync(&mut space, &m, None, &options());
    assert_eq!(first.chats.messages, 2);

    // Nothing new: nothing written.
    let again = sync(&mut space, &m, Some(first.cursor), &options());
    assert_eq!(again.chats.messages, 0);
    assert!(again.chats.chats.is_empty());
    assert_eq!(again.cursor, first.cursor);

    m.message(ADA_CHAT, None, "2026-03-04T09:30", "Third");
    m.message(ADA_CHAT, Some(ADA), "2026-03-06T07:00", "Fourth");
    let next = sync(&mut space, &m, Some(first.cursor), &options());
    assert_eq!(next.chats.messages, 2);
    assert_eq!(next.cursor, Cursor(4));
    let ada = chat_node(&space, ADA_CHAT);
    assert_eq!(
        ada.body,
        "## 2026-03-04\n\n\
         **+15550100** (09:15): First\n\n\
         **Me** (09:30): Third\n\n\
         ## 2026-03-06\n\n\
         **+15550100** (07:00): Fourth\n"
    );
    assert_eq!(prop(&space, &ada, chat_keys::MESSAGES), "3");

    // A restart that lost its cursor, or kept an old one, writes nothing twice.
    for cursor in [None, Some(Cursor(0)), Some(first.cursor)] {
        let report = sync(&mut space, &m, cursor, &options());
        assert_eq!(report.chats.messages, 0, "{cursor:?}");
    }
    assert_eq!(chat_node(&space, ADA_CHAT).body, ada.body);
    assert_eq!(imessage::cursor_in(&space).unwrap(), Some(Cursor(4)));
    assert_eq!(chat_count(&space), 2);
}

#[test]
fn excluded_conversations_are_never_read() {
    let m = two_conversations();
    m.message(ADA_CHAT, Some(ADA), "2026-03-04T09:15", "Keep this");
    m.message(TRIP_CHAT, Some(GRACE), "2026-03-04T09:20", "Not this");
    let options = SyncOptions {
        excluded: BTreeSet::from([TRIP_CHAT.to_string()]),
        ..options()
    };
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let report = sync(&mut space, &m, None, &options);
    assert_eq!(report.excluded, 1);
    assert_eq!(chat_count(&space), 1);
    assert!(
        space
            .find_by_property(chat_keys::ID, TRIP_CHAT)
            .unwrap()
            .is_empty()
    );

    // A new conversation comes in; the excluded one still does not.
    m.chat("iMessage;-;grace@example.com", 45, "", &[GRACE]);
    m.message(
        "iMessage;-;grace@example.com",
        Some(GRACE),
        "2026-03-04T11:00",
        "New",
    );
    m.message(TRIP_CHAT, Some(LINUS), "2026-03-04T11:01", "Still not this");
    sync(&mut space, &m, Some(report.cursor), &options);
    assert_eq!(chat_count(&space), 2);
    assert!(
        space
            .find_by_property(chat_keys::ID, TRIP_CHAT)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn from_today_brings_only_what_is_sent_from_today_on() {
    let m = two_conversations();
    m.message(ADA_CHAT, Some(ADA), "2026-03-03T23:59", "Yesterday");
    m.message(TRIP_CHAT, Some(GRACE), "2026-03-02T10:00", "Before");
    m.message(ADA_CHAT, Some(ADA), "2026-03-04T00:00", "Today");
    let now = DateTime::parse_from_rfc3339("2026-03-04T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let options = SyncOptions {
        history: History::FromToday,
        now: Some(now),
        ..options()
    };
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let report = sync(&mut space, &m, None, &options);
    assert_eq!(report.chats.messages, 1);
    assert_eq!(report.cursor, Cursor(3));
    assert_eq!(chat_count(&space), 1);
    assert_eq!(
        chat_node(&space, ADA_CHAT).body,
        "## 2026-03-04\n\n**+15550100** (00:00): Today\n"
    );

    // Later messages come in whatever their date says.
    m.message(TRIP_CHAT, Some(GRACE), "2026-03-04T13:00", "Later");
    let later = sync(&mut space, &m, Some(report.cursor), &options);
    assert_eq!(later.chats.messages, 1);
    assert_eq!(chat_count(&space), 2);
}

#[test]
fn from_today_with_nothing_today_starts_after_the_last_message() {
    let m = two_conversations();
    m.message(ADA_CHAT, Some(ADA), "2026-03-01T09:00", "Old");
    let now = DateTime::parse_from_rfc3339("2026-03-04T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let options = SyncOptions {
        history: History::FromToday,
        now: Some(now),
        ..options()
    };
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let report = sync(&mut space, &m, None, &options);
    assert_eq!(report.chats.messages, 0);
    assert_eq!(report.cursor, Cursor(1));
    assert_eq!(chat_count(&space), 0);
}

#[test]
fn a_changed_database_writes_nothing_and_asks_for_an_update() {
    let m = two_conversations();
    m.message(ADA_CHAT, Some(ADA), "2026-03-04T09:15", "Hello");
    m.conn
        .execute_batch(
            "ALTER TABLE message DROP COLUMN attributedBody; DROP TABLE chat_handle_join;",
        )
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let error = imessage::sync(&mut space, &m.source(), None, &options(), &by()).unwrap_err();
    match &error {
        IMessageError::NeedsUpdate { missing } => {
            assert_eq!(
                missing,
                &["message.attributedBody", "table chat_handle_join"]
            );
        }
        other => panic!("expected NeedsUpdate, got {other:?}"),
    }
    assert!(error.to_string().contains("needs an update"), "{error}");
    assert_eq!(chat_count(&space), 0);
    assert!(space.recent(10).unwrap().is_empty());
}

#[test]
fn says_when_full_disk_access_is_missing() {
    use std::os::unix::fs::PermissionsExt;
    let m = two_conversations();
    let db = m.dir.path().join("chat.db");
    fs::set_permissions(&db, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(&db).is_ok() {
        // Running as root: permissions do not stop anything.
        fs::set_permissions(&db, fs::Permissions::from_mode(0o644)).unwrap();
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let error = imessage::sync(&mut space, &m.source(), None, &options(), &by()).unwrap_err();
    fs::set_permissions(&db, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        matches!(error, IMessageError::NoFullDiskAccess(_)),
        "{error:?}"
    );
    assert!(error.to_string().contains("Full Disk Access"), "{error}");
    assert_eq!(chat_count(&space), 0);
}

#[test]
fn says_when_there_is_no_messages_database() {
    let empty = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let error = imessage::sync(
        &mut space,
        &MessagesSource::new(empty.path()),
        None,
        &options(),
        &by(),
    )
    .unwrap_err();
    assert!(matches!(error, IMessageError::NotFound(_)), "{error:?}");
}

struct Book;

impl ContactNames for Book {
    fn name(&self, handle: &str) -> Option<String> {
        match handle {
            ADA => Some("Ada Lovelace".into()),
            GRACE => Some("Grace Hopper".into()),
            _ => None,
        }
    }
}

#[test]
fn names_people_from_contacts_when_given() {
    let m = two_conversations();
    m.message(ADA_CHAT, Some(ADA), "2026-03-04T09:15", "Hi");
    m.message(TRIP_CHAT, Some(LINUS), "2026-03-04T09:16", "Hey");
    let book = Book;
    let options = SyncOptions {
        names: Some(&book),
        me: "Example".into(),
        ..options()
    };
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    sync(&mut space, &m, None, &options);
    let ada = chat_node(&space, ADA_CHAT);
    assert_eq!(ada.title, "iMessage - Ada Lovelace");
    assert_eq!(ada.body, "## 2026-03-04\n\n**Ada Lovelace** (09:15): Hi\n");
    let trip = chat_node(&space, TRIP_CHAT);
    assert_eq!(
        prop(&space, &trip, chat_keys::PARTICIPANTS),
        "Grace Hopper, +15550111, Example"
    );

    let listed = imessage::conversations(&m.source(), Some(&book)).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].guid, ADA_CHAT);
    assert_eq!(listed[0].title, "iMessage - Ada Lovelace");
    assert_eq!(listed[1].kind, "group");
}

#[test]
fn long_attributed_bodies_are_read() {
    let m = two_conversations();
    let text = "A long plan for the trip, day by day. ".repeat(10);
    m.insert(
        ADA_CHAT,
        Some(ADA),
        "2026-03-04T09:15",
        None,
        Some(attributed_body(&text)),
        &[],
    );
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    sync(&mut space, &m, None, &options());
    assert!(chat_node(&space, ADA_CHAT).body.contains(text.trim()));
}
