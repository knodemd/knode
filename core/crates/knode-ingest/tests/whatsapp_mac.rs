//! The WhatsApp for Mac connector, against synthetic databases built here with the tables and
//! columns WhatsApp for Mac uses: invented chats between invented people. No real database is
//! ever opened.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{Duration, Local, NaiveDate, NaiveDateTime, TimeZone};
use knode_format::{Node, Provenance, Space};
use knode_ingest::whatsapp_mac::{Cursor, History, Options, WhatsAppMac, WhatsAppMacError};
use knode_ingest::{chat_keys, import_chat_export};
use rusqlite::{Connection, params};
use tempfile::TempDir;

const APPLE_EPOCH: i64 = 978_307_200;

const ADA: &str = "34600000001@s.whatsapp.net";
const GRACE: &str = "34600000002@s.whatsapp.net";
const LIN: &str = "34600000003@s.whatsapp.net";
const CLUB: &str = "120363000000000001@g.us";

fn by() -> Provenance {
    Provenance::person("example")
}

fn space(dir: &TempDir) -> Space {
    Space::create(dir.path().join("Test.knode"), "Test").unwrap()
}

fn everything() -> Options {
    Options {
        history: History::Everything,
        ..Options::default()
    }
}

/// Apple's clock for a time in the local time zone.
fn apple(time: NaiveDateTime) -> f64 {
    (Local
        .from_local_datetime(&time)
        .earliest()
        .unwrap()
        .timestamp()
        - APPLE_EPOCH) as f64
}

fn at(date: NaiveDate, h: u32, m: u32) -> NaiveDateTime {
    date.and_hms_opt(h, m, 0).unwrap()
}

/// A synthetic WhatsApp for Mac container.
struct Fake {
    _dir: TempDir,
    container: PathBuf,
    db: Connection,
    next_message: i64,
}

impl Fake {
    fn new() -> Fake {
        Fake::with_schema(SCHEMA)
    }

    fn with_schema(schema: &str) -> Fake {
        let dir = tempfile::tempdir().unwrap();
        let container = dir.path().join("group.net.whatsapp.WhatsApp.shared");
        fs::create_dir_all(container.join("Message/Media")).unwrap();
        let db = Connection::open(container.join("ChatStorage.sqlite")).unwrap();
        db.execute_batch(schema).unwrap();
        Fake {
            _dir: dir,
            container,
            db,
            next_message: 1,
        }
    }

    fn app(&self) -> WhatsAppMac {
        WhatsAppMac::new(&self.container)
    }

    fn chat(&self, pk: i64, jid: &str, name: &str, group: bool) {
        self.db
            .execute(
                "INSERT INTO ZWACHATSESSION (Z_PK, Z_ENT, ZCONTACTJID, ZPARTNERNAME, ZGROUPINFO) \
                 VALUES (?1, 1, ?2, ?3, ?4)",
                params![pk, jid, name, group.then_some(pk)],
            )
            .unwrap();
    }

    fn member(&self, pk: i64, chat: i64, jid: &str) {
        self.db
            .execute(
                "INSERT INTO ZWAGROUPMEMBER (Z_PK, ZCHATSESSION, ZMEMBERJID) VALUES (?1, ?2, ?3)",
                params![pk, chat, jid],
            )
            .unwrap();
    }

    fn push_name(&self, jid: &str, name: &str) {
        self.db
            .execute(
                "INSERT INTO ZWAPROFILEPUSHNAME (ZJID, ZPUSHNAME) VALUES (?1, ?2)",
                params![jid, name],
            )
            .unwrap();
    }

    /// Adds a message and returns its primary key.
    #[allow(clippy::too_many_arguments)]
    fn message_full(
        &mut self,
        chat: i64,
        from_me: bool,
        member: Option<i64>,
        time: NaiveDateTime,
        kind: i64,
        text: Option<&str>,
    ) -> i64 {
        let pk = self.next_message;
        self.next_message += 1;
        self.db
            .execute(
                "INSERT INTO ZWAMESSAGE (Z_PK, ZCHATSESSION, ZISFROMME, ZGROUPMEMBER, \
                 ZMESSAGEDATE, ZMESSAGETYPE, ZTEXT, ZSTANZAID) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    pk,
                    chat,
                    from_me as i64,
                    member,
                    apple(time),
                    kind,
                    text,
                    format!("3EB0{pk:016X}")
                ],
            )
            .unwrap();
        pk
    }

    fn text(&mut self, chat: i64, from_me: bool, time: NaiveDateTime, text: &str) -> i64 {
        self.message_full(chat, from_me, None, time, 0, Some(text))
    }

    fn media(
        &self,
        message: i64,
        path: Option<&str>,
        title: Option<&str>,
        metadata: Option<&[u8]>,
    ) {
        self.db
            .execute(
                "INSERT INTO ZWAMEDIAITEM (ZMESSAGE, ZMEDIALOCALPATH, ZTITLE, ZMETADATA) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![message, path, title, metadata],
            )
            .unwrap();
    }
}

/// The tables and columns of WhatsApp's database that the connector reads, and a few more it
/// does not, as Core Data writes them.
const SCHEMA: &str = "
CREATE TABLE ZWACHATSESSION (Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, Z_OPT INTEGER,
    ZARCHIVED INTEGER, ZSESSIONTYPE INTEGER, ZGROUPINFO INTEGER, ZLASTMESSAGEDATE TIMESTAMP,
    ZCONTACTJID VARCHAR, ZPARTNERNAME VARCHAR);
CREATE TABLE ZWAMESSAGE (Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, Z_OPT INTEGER,
    ZISFROMME INTEGER, ZMESSAGETYPE INTEGER, ZCHATSESSION INTEGER, ZGROUPMEMBER INTEGER,
    ZMEDIAITEM INTEGER, ZMESSAGEDATE TIMESTAMP, ZSENTDATE TIMESTAMP, ZFROMJID VARCHAR,
    ZTOJID VARCHAR, ZSTANZAID VARCHAR, ZTEXT VARCHAR);
CREATE TABLE ZWAMEDIAITEM (Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, Z_OPT INTEGER,
    ZMESSAGE INTEGER, ZFILESIZE INTEGER, ZMEDIALOCALPATH VARCHAR, ZMEDIAURL VARCHAR,
    ZTITLE VARCHAR, ZVCARDSTRING VARCHAR, ZMETADATA BLOB);
CREATE TABLE ZWAGROUPMEMBER (Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, Z_OPT INTEGER,
    ZISADMIN INTEGER, ZCHATSESSION INTEGER, ZMEMBERJID VARCHAR, ZCONTACTNAME VARCHAR);
CREATE TABLE ZWAPROFILEPUSHNAME (Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, Z_OPT INTEGER,
    ZJID VARCHAR, ZPUSHNAME VARCHAR);
";

fn whatsapp_nodes(space: &Space) -> Vec<Node> {
    space
        .find_by_property(chat_keys::APP, "whatsapp")
        .unwrap()
        .into_iter()
        .map(|s| space.get_node(&s.id).unwrap().unwrap())
        .collect()
}

fn node_with_jid(space: &Space, jid: &str) -> Node {
    let found = space.find_by_property(chat_keys::JID, jid).unwrap();
    assert_eq!(found.len(), 1, "one node for {jid}");
    space.get_node(&found[0].id).unwrap().unwrap()
}

fn prop(space: &Space, node: &Node, key: &str) -> String {
    space.property(&node.id, key).unwrap().unwrap_or_default()
}

fn yesterday() -> NaiveDate {
    Local::now().date_naive() - Duration::days(1)
}

#[test]
fn brings_only_new_messages_and_never_twice() {
    let mut fake = Fake::new();
    fake.chat(1, ADA, "Ada Lovelace", false);
    let day = yesterday();
    fake.text(
        1,
        false,
        at(day, 9, 15),
        "Morning! Shall we review the notes?",
    );
    fake.text(1, true, at(day, 9, 16), "Yes, at ten");

    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let app = fake.app();
    let first = app
        .sync(&mut space, Cursor::default(), &everything(), &by())
        .unwrap();
    assert_eq!(first.read, 2);
    assert_eq!(first.import.messages, 2);
    assert_eq!(first.cursor, Cursor::after(2));

    let node = node_with_jid(&space, ADA);
    assert_eq!(node.title, "WhatsApp - Ada Lovelace");
    assert_eq!(prop(&space, &node, chat_keys::ID), "ada lovelace");
    assert_eq!(prop(&space, &node, chat_keys::TYPE), "personal");
    assert_eq!(prop(&space, &node, chat_keys::MESSAGES), "2");
    assert!(
        node.body.contains("**Ada Lovelace** (09:15): Morning!"),
        "{}",
        node.body
    );
    assert!(
        node.body.contains("**Me** (09:16): Yes, at ten"),
        "{}",
        node.body
    );

    // A new message: only it is read.
    fake.text(1, false, at(day, 9, 16), "See you then");
    let saved = first.cursor.to_string();
    // Knode Service restarts: the cursor comes back from where it was kept.
    let cursor: Cursor = saved.parse().unwrap();
    let second = app.sync(&mut space, cursor, &everything(), &by()).unwrap();
    assert_eq!(second.read, 1);
    assert_eq!(second.import.messages, 1);
    assert_eq!(second.cursor, Cursor::after(3));

    // Nothing new: nothing read, nothing written.
    let third = app
        .sync(&mut space, second.cursor, &everything(), &by())
        .unwrap();
    assert_eq!(third.read, 0);
    assert!(third.import.chats.is_empty());

    // Even a lost cursor writes nothing twice.
    let again = app
        .sync(&mut space, Cursor::default(), &everything(), &by())
        .unwrap();
    assert_eq!(again.read, 3);
    assert_eq!(again.import.messages, 0);

    let node = node_with_jid(&space, ADA);
    assert_eq!(prop(&space, &node, chat_keys::MESSAGES), "3");
    assert_eq!(node.body.matches("See you then").count(), 1);
    assert_eq!(node.body.matches("Yes, at ten").count(), 1);
    assert_eq!(whatsapp_nodes(&space).len(), 1);
}

#[test]
fn reads_a_copy_with_what_the_log_holds_and_never_writes() {
    let mut fake = Fake::new();
    fake.db
        .execute_batch("PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;")
        .unwrap();
    fake.chat(1, ADA, "Ada Lovelace", false);
    fake.text(1, false, at(yesterday(), 8, 0), "Only in the log so far");
    let database = fake.container.join("ChatStorage.sqlite");
    let wal = fake.container.join("ChatStorage.sqlite-wal");
    assert!(
        fs::metadata(&wal).unwrap().len() > 0,
        "the message is in the log"
    );
    let before = (fs::read(&database).unwrap(), fs::read(&wal).unwrap());

    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let report = fake
        .app()
        .sync(&mut space, Cursor::default(), &everything(), &by())
        .unwrap();
    assert_eq!(report.import.messages, 1);
    assert!(
        node_with_jid(&space, ADA)
            .body
            .contains("Only in the log so far")
    );

    let after = (fs::read(&database).unwrap(), fs::read(&wal).unwrap());
    assert!(before == after, "the database and its log are untouched");
}

#[test]
fn excluded_chats_are_never_read() {
    let mut fake = Fake::new();
    fake.chat(1, ADA, "Ada Lovelace", false);
    fake.chat(2, GRACE, "Grace Hopper", false);
    let day = yesterday();
    fake.text(1, false, at(day, 9, 0), "Hello from Ada");
    fake.text(2, false, at(day, 9, 1), "Hello from Grace");
    let options = Options {
        excluded: [GRACE.to_string()].into(),
        ..everything()
    };

    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let app = fake.app();
    let chats = app.chats().unwrap();
    assert_eq!(
        chats.iter().map(|c| c.jid.as_str()).collect::<Vec<_>>(),
        [ADA, GRACE]
    );
    let first = app
        .sync(&mut space, Cursor::default(), &options, &by())
        .unwrap();
    assert_eq!(first.read, 1);
    fake.text(2, false, at(day, 9, 2), "Still excluded");
    let second = app.sync(&mut space, first.cursor, &options, &by()).unwrap();
    assert_eq!(second.read, 0);
    // The cursor moves past the excluded chat's messages all the same.
    assert_eq!(second.cursor, Cursor::after(3));

    let nodes = whatsapp_nodes(&space);
    assert_eq!(nodes.len(), 1);
    assert!(
        space
            .find_by_property(chat_keys::JID, GRACE)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn the_first_sync_reads_from_today_or_everything() {
    let mut fake = Fake::new();
    fake.chat(1, ADA, "Ada Lovelace", false);
    let today = Local::now().date_naive();
    fake.text(
        1,
        false,
        at(today - Duration::days(30), 12, 0),
        "Last month",
    );
    let now = Local::now().naive_local() - Duration::minutes(1);
    fake.text(1, false, now.max(at(today, 0, 0)), "Today");

    let dir = tempfile::tempdir().unwrap();
    let mut from_today = space(&dir);
    let report = fake
        .app()
        .sync(
            &mut from_today,
            Cursor::default(),
            &Options::default(),
            &by(),
        )
        .unwrap();
    assert_eq!(report.read, 1);
    let body = node_with_jid(&from_today, ADA).body;
    assert!(
        body.contains("Today") && !body.contains("Last month"),
        "{body}"
    );
    // Later syncs read what is new, whatever its date.
    fake.text(
        1,
        false,
        at(today - Duration::days(2), 12, 0),
        "Restored late",
    );
    let later = fake
        .app()
        .sync(&mut from_today, report.cursor, &Options::default(), &by())
        .unwrap();
    assert_eq!(later.read, 1);

    let other = tempfile::tempdir().unwrap();
    let mut all = space(&other);
    let report = fake
        .app()
        .sync(&mut all, Cursor::default(), &everything(), &by())
        .unwrap();
    assert_eq!(report.read, 3);
    let body = node_with_jid(&all, ADA).body;
    assert!(
        body.contains("Last month") && body.contains("Today"),
        "{body}"
    );
}

#[test]
fn an_export_and_the_connector_share_one_node() {
    let day = NaiveDate::from_ymd_opt(2026, 3, 14).unwrap();
    let export = tempfile::tempdir().unwrap();
    let txt = export.path().join("WhatsApp Chat with Ada Lovelace.txt");
    fs::write(
        &txt,
        "14/03/2026, 09:15 - Ada Lovelace: Morning\n\
         14/03/2026, 09:16 - Me: Hi\n",
    )
    .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let imported = import_chat_export(&mut space, &txt, None, &by()).unwrap();
    assert_eq!(imported.messages, 2);

    let mut fake = Fake::new();
    fake.chat(1, ADA, "Ada Lovelace", false);
    fake.text(1, false, at(day, 9, 15), "Morning");
    fake.text(1, true, at(day, 9, 16), "Hi");
    fake.text(1, false, at(day, 9, 20), "Coffee?");
    let report = fake
        .app()
        .sync(&mut space, Cursor::default(), &everything(), &by())
        .unwrap();
    assert_eq!(
        report.import.messages, 1,
        "only the message after the export"
    );

    let nodes = whatsapp_nodes(&space);
    assert_eq!(nodes.len(), 1);
    let node = &nodes[0];
    assert_eq!(prop(&space, node, chat_keys::JID), ADA);
    assert_eq!(prop(&space, node, chat_keys::MESSAGES), "3");
    assert_eq!(node.body.matches("Morning").count(), 1, "{}", node.body);
    assert_eq!(node.body.matches(": Hi").count(), 1, "{}", node.body);
    assert!(node.body.contains("Coffee?"));

    // A later export of the chat finds the node by its name, and adds only what is new.
    fs::write(
        &txt,
        "14/03/2026, 09:15 - Ada Lovelace: Morning\n\
         14/03/2026, 09:16 - Me: Hi\n\
         14/03/2026, 09:20 - Ada Lovelace: Coffee?\n\
         14/03/2026, 09:30 - Me: Sure\n",
    )
    .unwrap();
    let again = import_chat_export(&mut space, &txt, None, &by()).unwrap();
    assert_eq!(again.messages, 1);
    assert_eq!(whatsapp_nodes(&space).len(), 1);
    assert_eq!(
        prop(&space, &node_with_jid(&space, ADA), chat_keys::MESSAGES),
        "4"
    );
}

#[test]
fn a_chat_of_the_same_name_with_another_jid_is_another_node() {
    let mut fake = Fake::new();
    fake.chat(1, ADA, "Sam", false);
    fake.chat(2, LIN, "Sam", false);
    let day = yesterday();
    fake.text(1, false, at(day, 9, 0), "One Sam");
    fake.text(2, false, at(day, 9, 1), "Another Sam");
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    fake.app()
        .sync(&mut space, Cursor::default(), &everything(), &by())
        .unwrap();
    assert!(node_with_jid(&space, ADA).body.contains("One Sam"));
    assert!(node_with_jid(&space, LIN).body.contains("Another Sam"));
}

#[test]
fn groups_media_replies_and_what_whatsapp_writes() {
    let mut fake = Fake::new();
    fake.chat(1, CLUB, "Book Club", true);
    fake.chat(2, ADA, "Ada Lovelace", false);
    fake.member(10, 1, ADA);
    fake.member(11, 1, GRACE);
    fake.member(12, 1, LIN);
    fake.push_name(GRACE, "Grace");
    let day = yesterday();
    let question = fake.message_full(
        1,
        false,
        Some(10),
        at(day, 18, 0),
        0,
        Some("Which book next?"),
    );
    let answer = fake.message_full(1, false, Some(11), at(day, 18, 5), 0, Some("Dune"));
    let mut metadata = vec![0x2a, 0x14];
    metadata.extend_from_slice(format!("3EB0{question:016X}").as_bytes());
    fake.media(answer, None, None, Some(&metadata));
    fake.message_full(1, false, Some(12), at(day, 18, 6), 0, Some("Fine by me"));
    fake.message_full(1, false, None, at(day, 18, 7), 6, Some("Sci-fi Club"));
    fake.message_full(1, false, None, at(day, 18, 8), 6, Some("3"));
    let photo = fake.message_full(1, true, None, at(day, 18, 10), 1, None);
    let file = "Media/120363000000000001@g.us/a/b/cover.jpg";
    fs::create_dir_all(
        fake.container
            .join("Message/Media/120363000000000001@g.us/a/b"),
    )
    .unwrap();
    fs::write(
        fake.container.join("Message").join(file),
        b"synthetic image",
    )
    .unwrap();
    fake.media(photo, Some(file), Some("The cover"), None);
    let voice = fake.message_full(1, false, Some(10), at(day, 18, 12), 3, None);
    fake.media(
        voice,
        Some("Media/120363000000000001@g.us/c/d/voice.opus"),
        None,
        None,
    );
    let doc = fake.message_full(1, false, Some(11), at(day, 18, 14), 8, None);
    fake.media(doc, None, Some("Reading list.pdf"), None);
    fake.message_full(1, false, Some(10), at(day, 18, 15), 14, None);

    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let report = fake
        .app()
        .sync(&mut space, Cursor::default(), &everything(), &by())
        .unwrap();
    assert_eq!(report.import.attached, 1);

    let node = node_with_jid(&space, CLUB);
    let body = &node.body;
    assert_eq!(prop(&space, &node, chat_keys::TYPE), "group");
    assert!(
        body.contains("**Ada Lovelace** (18:00): Which book next?"),
        "{body}"
    );
    assert!(
        body.contains(
            "**Grace** (18:05, replying to Ada Lovelace at 18:00: \"Which book next?\"): Dune"
        ),
        "{body}"
    );
    assert!(
        body.contains("**+34600000003** (18:06): Fine by me"),
        "{body}"
    );
    assert!(
        body.contains("*(18:07) The group's name changed to Sci-fi Club*"),
        "{body}"
    );
    assert!(!body.contains("(18:08)"), "{body}");
    assert!(
        body.contains("**Me** (18:10): The cover\n![[cover.jpg]]"),
        "{body}"
    );
    assert!(body.contains("*[not on this Mac: voice.opus]*"), "{body}");
    assert!(
        body.contains("*[not on this Mac: Reading list.pdf]*"),
        "{body}"
    );
    assert!(body.contains("(18:15): This message was deleted"), "{body}");
    let attachments = space.attachments(&node.id).unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(
        prop(&space, &node, chat_keys::PARTICIPANTS),
        "Ada Lovelace, Grace, +34600000003, Me"
    );
}

#[test]
fn a_changed_schema_writes_nothing() {
    let schema = SCHEMA.replace(
        "ZTOJID VARCHAR, ZSTANZAID VARCHAR, ZTEXT VARCHAR",
        "ZTOJID VARCHAR, ZSTANZAID VARCHAR, ZBODY VARCHAR",
    );
    let fake = Fake::with_schema(&schema);
    fake.chat(1, ADA, "Ada Lovelace", false);
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let error = fake
        .app()
        .sync(&mut space, Cursor::default(), &everything(), &by())
        .unwrap_err();
    assert!(
        matches!(&error, WhatsAppMacError::NeedsUpdate { missing } if missing == &["ZWAMESSAGE.ZTEXT"])
    );
    let message = error.to_string();
    assert!(
        message.contains("needs an update") && message.contains("Nothing was written"),
        "{message}"
    );
    assert!(whatsapp_nodes(&space).is_empty());

    let without_table = SCHEMA.replace("CREATE TABLE ZWAPROFILEPUSHNAME", "CREATE TABLE ZOTHER");
    let fake = Fake::with_schema(&without_table);
    let error = fake.app().check().unwrap_err();
    assert!(
        matches!(&error, WhatsAppMacError::NeedsUpdate { missing } if missing == &["ZWAPROFILEPUSHNAME"])
    );
}

#[test]
fn says_when_whatsapp_is_not_there() {
    let dir = tempfile::tempdir().unwrap();
    let app = WhatsAppMac::for_home(dir.path());
    let error = app.check().unwrap_err();
    assert!(matches!(error, WhatsAppMacError::NotInstalled(_)));
    assert!(
        error.to_string().contains("WhatsApp for Mac was not found"),
        "{error}"
    );

    // The container without its database: the app was never linked.
    let container: &Path = app.container();
    fs::create_dir_all(container).unwrap();
    assert!(matches!(
        app.check().unwrap_err(),
        WhatsAppMacError::NotInstalled(_)
    ));
}

#[cfg(unix)]
#[test]
fn says_when_full_disk_access_is_missing() {
    use std::os::unix::fs::PermissionsExt;
    let fake = Fake::new();
    let database = fake.container.join("ChatStorage.sqlite");
    fs::set_permissions(&database, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(&database).is_ok() {
        // Running as a user who reads every file: nothing to check.
        return;
    }
    let error = fake.app().check().unwrap_err();
    fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        matches!(error, WhatsAppMacError::NoFullDiskAccess(_)),
        "{error:?}"
    );
    assert!(error.to_string().contains("Full Disk Access"), "{error}");
}

mod connector {
    use super::*;
    use knode_format::Timestamp;
    use knode_ingest::sources::{
        Connector, Container, Cursor as SourceCursor, FetchRequest, History as SourceHistory,
        NoSecrets, Permission, SourceError, Trigger, provenance,
    };
    use knode_ingest::whatsapp_mac::WhatsAppMacConnector;

    fn request<'a>(
        cursor: Option<&'a SourceCursor>,
        history: SourceHistory,
        since: Timestamp,
        excluded: &'a [Container],
        max_items: usize,
    ) -> FetchRequest<'a> {
        FetchRequest {
            space_id: "space",
            cursor,
            history,
            since,
            excluded,
            max_items,
            secrets: &NoSecrets,
        }
    }

    fn start_of_today_ms() -> Timestamp {
        let midnight = at(Local::now().date_naive(), 0, 0);
        Timestamp(
            Local
                .from_local_datetime(&midnight)
                .earliest()
                .unwrap()
                .timestamp_millis(),
        )
    }

    #[test]
    fn describes_itself() {
        let fake = Fake::new();
        let connector = WhatsAppMacConnector::new(fake.app(), "Me");
        let info = connector.info();
        assert_eq!(info.id, "whatsapp");
        assert_eq!(info.permissions, [Permission::FullDiskAccess]);
        assert_eq!(info.container.as_deref(), Some("chat"));
        match connector.trigger() {
            Trigger::Watch { paths, every } => {
                assert!(paths[0].ends_with("ChatStorage.sqlite"));
                assert!(paths[1].ends_with("ChatStorage.sqlite-wal"));
                assert!(every <= std::time::Duration::from_secs(60));
            }
            other => panic!("{other:?}"),
        }
        connector.check().unwrap();
    }

    #[test]
    fn fetches_in_batches_and_writes_once() {
        let mut fake = Fake::new();
        fake.chat(1, ADA, "Ada Lovelace", false);
        fake.chat(2, GRACE, "Grace Hopper", false);
        let day = yesterday();
        for m in 0..5 {
            fake.text(1, false, at(day, 10, m), &format!("Ada {m}"));
        }
        fake.text(2, false, at(day, 11, 0), "Grace, excluded");
        let connector = WhatsAppMacConnector::new(fake.app(), "Me");
        let excluded = [Container::new(GRACE, "Grace Hopper")];
        let containers = connector
            .containers(&request(
                None,
                SourceHistory::Everything,
                Timestamp(0),
                &[],
                10,
            ))
            .unwrap();
        assert_eq!(containers.len(), 2);

        let dir = tempfile::tempdir().unwrap();
        let mut space = space(&dir);
        let by = provenance("whatsapp");
        let mut cursor: Option<SourceCursor> = None;
        let mut items = 0;
        let mut runs = 0;
        loop {
            let fetched = connector
                .fetch(&request(
                    cursor.as_ref(),
                    SourceHistory::Everything,
                    Timestamp(0),
                    &excluded,
                    2,
                ))
                .unwrap();
            runs += 1;
            if let Some(batch) = fetched.batch {
                assert!(batch.len() <= 2);
                // A run cut short before its cursor is saved writes the batch again: nothing
                // is added twice.
                items += batch.write(&mut space, &by).unwrap().items;
            }
            cursor = Some(fetched.cursor);
            if !fetched.more {
                break;
            }
        }
        assert_eq!(items, 5);
        assert_eq!(runs, 3);
        assert_eq!(cursor.as_ref().unwrap().as_str(), "6");
        let body = node_with_jid(&space, ADA).body;
        for m in 0..5 {
            assert_eq!(body.matches(&format!("Ada {m}")).count(), 1, "{body}");
        }
        assert!(
            space
                .find_by_property(chat_keys::JID, GRACE)
                .unwrap()
                .is_empty()
        );

        // Read again from the start: the importer adds nothing.
        let again = connector
            .fetch(&request(
                None,
                SourceHistory::Everything,
                Timestamp(0),
                &excluded,
                100,
            ))
            .unwrap();
        let written = again.batch.unwrap().write(&mut space, &by).unwrap();
        assert_eq!(written.items, 0);
    }

    #[test]
    fn from_today_never_reads_older_messages() {
        let mut fake = Fake::new();
        fake.chat(1, ADA, "Ada Lovelace", false);
        let today = Local::now().date_naive();
        let now = (Local::now().naive_local() - Duration::minutes(1)).max(at(today, 0, 0));
        fake.text(1, false, now, "Today");
        let connector = WhatsAppMacConnector::new(fake.app(), "Me");
        let since = start_of_today_ms();
        let first = connector
            .fetch(&request(None, SourceHistory::FromToday, since, &[], 100))
            .unwrap();
        assert_eq!(first.batch.as_ref().map(|b| b.len()), Some(1));
        fake.text(
            1,
            false,
            at(today - Duration::days(3), 9, 0),
            "Restored, older",
        );
        let second = connector
            .fetch(&request(
                Some(&first.cursor),
                SourceHistory::FromToday,
                since,
                &[],
                100,
            ))
            .unwrap();
        assert!(second.batch.is_none());
        assert_eq!(second.cursor.as_str(), "2");
    }

    #[test]
    fn reports_errors_as_the_service_shows_them() {
        let dir = tempfile::tempdir().unwrap();
        let connector = WhatsAppMacConnector::new(WhatsAppMac::for_home(dir.path()), "Me");
        let error = connector.check().unwrap_err();
        assert!(matches!(error, SourceError::Unavailable(_)), "{error:?}");
        assert!(error.needs_person());

        let schema = SCHEMA.replace("ZMESSAGETYPE INTEGER, ", "");
        let fake = Fake::with_schema(&schema);
        let connector = WhatsAppMacConnector::new(fake.app(), "Me");
        let error = connector
            .fetch(&request(
                None,
                SourceHistory::Everything,
                Timestamp(0),
                &[],
                10,
            ))
            .unwrap_err();
        assert_eq!(error.kind(), "needs-update");
        assert!(
            error.to_string().contains("ZWAMESSAGE.ZMESSAGETYPE"),
            "{error}"
        );

        let fake = Fake::new();
        let connector = WhatsAppMacConnector::new(fake.app(), "Me");
        let bad = SourceCursor::new("not a number");
        let error = connector
            .fetch(&request(
                Some(&bad),
                SourceHistory::Everything,
                Timestamp(0),
                &[],
                10,
            ))
            .unwrap_err();
        assert_eq!(error.kind(), "failed");
    }
}
