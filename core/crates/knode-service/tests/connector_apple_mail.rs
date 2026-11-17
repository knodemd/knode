//! The Apple Mail connector over a synthetic Mail store written here: an Envelope Index with
//! the tables and columns Mail's has, and `.emlx` files laid out as Mail lays them out, for a
//! Gmail account, an iCloud account and mailboxes On My Mac. Every person, address and account
//! is invented; no real Mail store is read.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use knode_format::{Provenance, Space, Timestamp};
use knode_service::sources::apple_mail::{AppleMailConnector, ID};
use knode_service::sources::{
    Connector, Container, Cursor, FetchRequest, History, NoSecrets, Permission, SourceError,
    SourcesOptions, Trigger, Written, provenance, start_of_today,
};
use knode_service::{Client, Config, start};
use knode_vault::mail::{MailOptions, MailSource, import_mail, keys, kinds};
use rusqlite::Connection;
use tempfile::TempDir;

const GMAIL: &str = "4A1B2C3D-0000-4000-8000-00000000A001";
const ICLOUD: &str = "7E8F9A0B-0000-4000-8000-00000000B002";
const DAY: i64 = 24 * 3600;

fn inbox(account: &str) -> String {
    format!("imap://{account}/INBOX")
}

fn all_mail() -> String {
    format!("imap://{GMAIL}/%5BGmail%5D/All%20Mail")
}

/// A synthetic `~/Library/Mail` with one store, `V10`.
struct Store {
    dir: TempDir,
    /// Kept open, as Mail keeps it: what it writes is in the `-wal` until a checkpoint.
    index: Connection,
    mailboxes: BTreeMap<String, i64>,
}

impl Store {
    fn new() -> Store {
        Store::with_schema(
            "CREATE TABLE messages (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, message_id INTEGER, \
             global_message_id INTEGER, remote_id INTEGER, sender INTEGER, subject INTEGER, \
             date_sent INTEGER, date_received INTEGER, mailbox INTEGER, remote_mailbox INTEGER, \
             flags INTEGER DEFAULT 0, read INTEGER DEFAULT 0, deleted INTEGER DEFAULT 0, \
             size INTEGER DEFAULT 0, conversation_id INTEGER DEFAULT 0);",
        )
    }

    fn with_schema(messages: &str) -> Store {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("Library/Mail/V10/MailData");
        fs::create_dir_all(&data).unwrap();
        // An older store Mail left behind is not read.
        fs::create_dir_all(dir.path().join("Library/Mail/V9/MailData")).unwrap();
        let index = Connection::open(data.join("Envelope Index")).unwrap();
        index
            .execute_batch(&format!(
                "PRAGMA journal_mode = WAL;
                 {messages}
                 CREATE TABLE mailboxes (ROWID INTEGER PRIMARY KEY, url TEXT UNIQUE, \
                 total_count INTEGER DEFAULT 0, unread_count INTEGER DEFAULT 0);
                 CREATE TABLE addresses (ROWID INTEGER PRIMARY KEY, address TEXT, comment TEXT);"
            ))
            .unwrap();
        let mut store = Store {
            dir,
            index,
            mailboxes: BTreeMap::new(),
        };
        for url in [
            inbox(GMAIL),
            all_mail(),
            format!("imap://{GMAIL}/%5BGmail%5D/Sent%20Mail"),
            format!("imap://{GMAIL}/%5BGmail%5D/Spam"),
            format!("imap://{GMAIL}/%5BGmail%5D/Trash"),
            inbox(ICLOUD),
            format!("imap://{ICLOUD}/Junk"),
            format!("imap://{ICLOUD}/Deleted%20Messages"),
            "local:///Archive".to_string(),
        ] {
            store.mailbox(&url);
        }
        store
    }

    fn mail_dir(&self) -> PathBuf {
        self.dir.path().join("Library/Mail")
    }

    fn store(&self) -> PathBuf {
        self.mail_dir().join("V10")
    }

    fn connector(&self) -> AppleMailConnector {
        AppleMailConnector::new(self.mail_dir())
    }

    fn mailbox(&mut self, url: &str) -> i64 {
        if let Some(id) = self.mailboxes.get(url) {
            return *id;
        }
        self.index
            .execute("INSERT INTO mailboxes (url) VALUES (?1)", [url])
            .unwrap();
        let id = self.index.last_insert_rowid();
        self.mailboxes.insert(url.to_string(), id);
        // Its folder: `<account>/<part>.mbox/...`, with the folder of its messages.
        fs::create_dir_all(
            self.folder(url)
                .join("1F2E3D4C-0000-4000-8000-0000000000C3/Data"),
        )
        .unwrap();
        id
    }

    fn folder(&self, url: &str) -> PathBuf {
        let (scheme, rest) = url.split_once("://").unwrap();
        let (host, path) = rest.split_once('/').unwrap();
        let mut folder = if scheme == "local" {
            self.store().join("Mailboxes")
        } else {
            self.store().join(host)
        };
        for part in path.split('/') {
            let part = part
                .replace("%5B", "[")
                .replace("%5D", "]")
                .replace("%20", " ");
            folder = folder.join(format!("{part}.mbox"));
        }
        folder
    }

    /// Where Mail writes the message numbered `rowid` of the mailbox.
    fn file(&self, url: &str, rowid: i64, partial: bool) -> PathBuf {
        let mut dir = self
            .folder(url)
            .join("1F2E3D4C-0000-4000-8000-0000000000C3/Data");
        if rowid >= 1000 {
            for d in (rowid / 1000).to_string().chars().rev() {
                dir = dir.join(d.to_string());
            }
        }
        let suffix = if partial { "partial.emlx" } else { "emlx" };
        dir.join("Messages").join(format!("{rowid}.{suffix}"))
    }

    /// Lists a message in the index, received at `received` (seconds since 1970), and returns
    /// its number. Its file is written only with `write`.
    fn list(&mut self, url: &str, received: i64, from: &str) -> i64 {
        let mailbox = self.mailbox(url);
        self.index
            .execute(
                "INSERT OR IGNORE INTO addresses (ROWID, address) \
                 SELECT IFNULL(MAX(ROWID), 0) + 1, ?1 FROM addresses \
                 WHERE NOT EXISTS (SELECT 1 FROM addresses WHERE address = ?1)",
                [from],
            )
            .unwrap();
        let sender: i64 = self
            .index
            .query_row(
                "SELECT ROWID FROM addresses WHERE address = ?1",
                [from],
                |r| r.get(0),
            )
            .unwrap();
        self.index
            .execute(
                "INSERT INTO messages (sender, date_sent, date_received, mailbox) \
                 VALUES (?1, ?2, ?2, ?3)",
                [sender, received, mailbox],
            )
            .unwrap();
        self.index.last_insert_rowid()
    }

    fn write(&self, url: &str, rowid: i64, message: &str) {
        let file = self.file(url, rowid, false);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, emlx(message)).unwrap();
    }

    /// Lists a message and writes its file.
    fn add(&mut self, url: &str, received: i64, message: &str) -> i64 {
        let from = header(message, "From");
        let rowid = self.list(url, received, &from);
        self.write(url, rowid, message);
        rowid
    }

    /// Closes the index and opens it again, as Mail does now and then: macOS reports the change
    /// of the files then.
    fn reopen(&mut self) {
        let path = self.store().join("MailData/Envelope Index");
        self.index = Connection::open(path).unwrap();
    }

    fn set_deleted(&self, rowid: i64) {
        self.index
            .execute("UPDATE messages SET deleted = 1 WHERE ROWID = ?1", [rowid])
            .unwrap();
    }
}

fn header(message: &str, name: &str) -> String {
    message
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{name}: ")))
        .map(|v| {
            v.rsplit('<')
                .next()
                .unwrap()
                .trim_end_matches('>')
                .to_string()
        })
        .unwrap_or_default()
}

/// An `.emlx` file: the length of the message, the message, then Mail's property list.
fn emlx(message: &str) -> Vec<u8> {
    let mut out = format!("{}\n", message.len()).into_bytes();
    out.extend_from_slice(message.as_bytes());
    out.extend_from_slice(
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
          <key>flags</key><integer>0</integer></dict></plist>\n",
    );
    out
}

fn message(id: &str, from: &str, subject: &str, at: i64, extra: &str) -> String {
    let date = rfc2822(at);
    format!(
        "From: {from}\r\nTo: Bruno Sample <bruno@example.org>\r\nSubject: {subject}\r\n\
         Date: {date}\r\nMessage-ID: <{id}>\r\n{extra}\r\nHello from {id}.\r\n"
    )
}

/// A time as a `Date:` header gives it, in UTC.
fn rfc2822(at: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = at.div_euclid(DAY);
    let secs = at.rem_euclid(DAY);
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{}, {day:02} {} {year} {:02}:{:02}:{:02} +0000",
        DAYS[days.rem_euclid(7) as usize],
        MONTHS[(month - 1) as usize],
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

const ADA: &str = "Ada Example <ada@gmail.example.com>";
const CLEO: &str = "Cleo Test <cleo@icloud.example.net>";

fn now() -> i64 {
    Timestamp::now().0 / 1000
}

fn long_ago() -> i64 {
    start_of_today().0 / 1000 - 400 * DAY
}

fn space(dir: &Path) -> Space {
    Space::create(dir.join("Personal.knode"), "Personal").unwrap()
}

/// What the service does in one run: check, then fetch and write until there is no more.
fn run(
    connector: &AppleMailConnector,
    space: &mut Space,
    cursor: &mut Option<Cursor>,
    history: History,
    excluded: &[Container],
) -> Result<Written, SourceError> {
    connector.check()?;
    let mut written = Written::default();
    loop {
        let request = FetchRequest {
            space_id: "space",
            cursor: cursor.as_ref(),
            history,
            since: start_of_today(),
            excluded,
            max_items: 2,
            secrets: &NoSecrets,
        };
        let fetched = connector.fetch(&request)?;
        if let Some(batch) = fetched.batch {
            written += batch.write(space, &provenance(ID))?;
        }
        *cursor = Some(fetched.cursor);
        if !fetched.more {
            return Ok(written);
        }
    }
}

/// The thread nodes of the space: title and message count.
fn threads(space: &Space) -> BTreeMap<String, usize> {
    space
        .recent(1000)
        .unwrap()
        .into_iter()
        .filter(|n| n.kind == kinds::THREAD)
        .map(|n| {
            let messages = space
                .property(&n.id, keys::MESSAGES)
                .unwrap()
                .unwrap_or_default()
                .parse()
                .unwrap_or(0);
            (n.title, messages)
        })
        .collect()
}

fn thread_body(space: &Space, title: &str) -> String {
    let node = space
        .find_by_title(title)
        .unwrap()
        .into_iter()
        .find(|n| n.kind == kinds::THREAD)
        .unwrap();
    space.get_node(&node.id).unwrap().unwrap().body
}

#[test]
fn from_today_brings_only_new_mail_of_every_account() {
    let mut store = Store::new();
    store.add(
        &inbox(GMAIL),
        long_ago(),
        &message("old@x", ADA, "Old news", long_ago(), ""),
    );
    store.add(
        &inbox(GMAIL),
        now(),
        &message("g1@x", ADA, "Gmail today", now(), ""),
    );
    // Gmail shows the same message in All Mail too: it comes in once.
    store.add(
        &all_mail(),
        now(),
        &message("g1@x", ADA, "Gmail today", now(), ""),
    );
    store.add(
        &inbox(ICLOUD),
        now(),
        &message("i1@x", CLEO, "iCloud today", now(), ""),
    );
    store.add(
        "local:///Archive",
        now(),
        &message("l1@x", CLEO, "Kept on my Mac", now(), ""),
    );
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let connector = store.connector();
    let mut cursor = None;

    let written = run(&connector, &mut space, &mut cursor, History::FromToday, &[]).unwrap();
    assert_eq!(written.items, 3);
    let found = threads(&space);
    assert_eq!(
        found.keys().cloned().collect::<Vec<_>>(),
        ["Gmail today", "Kept on my Mac", "iCloud today"]
    );
    assert!(found.values().all(|n| *n == 1));
    // The people of the threads came along.
    assert!(
        space
            .find_by_property(keys::PERSON_EMAIL, "cleo@icloud.example.net")
            .unwrap()
            .len()
            == 1
    );
    let node = space.find_by_title("Gmail today").unwrap().remove(0);
    assert_eq!(
        space.get_node(&node.id).unwrap().unwrap().created_by.actor,
        ID
    );

    // Only what is new afterwards.
    let rowid = store.add(
        &inbox(ICLOUD),
        now(),
        &message("i2@x", CLEO, "Later", now(), ""),
    );
    let written = run(&connector, &mut space, &mut cursor, History::FromToday, &[]).unwrap();
    assert_eq!(written.items, 1);
    assert!(threads(&space).contains_key("Later"));
    assert!(
        cursor
            .as_ref()
            .unwrap()
            .as_str()
            .contains(&format!("\"rowid\":{rowid}"))
    );
    // Nothing new: nothing written.
    let written = run(&connector, &mut space, &mut cursor, History::FromToday, &[]).unwrap();
    assert_eq!(written, Written::default());
}

#[test]
fn everything_brings_the_history_too() {
    let mut store = Store::new();
    store.add(
        &inbox(GMAIL),
        long_ago(),
        &message("old@x", ADA, "Old news", long_ago(), ""),
    );
    store.add(
        &inbox(GMAIL),
        now(),
        &message("g1@x", ADA, "Gmail today", now(), ""),
    );
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut cursor = None;
    let written = run(
        &store.connector(),
        &mut space,
        &mut cursor,
        History::Everything,
        &[],
    )
    .unwrap();
    assert_eq!(written.items, 2);
    assert!(threads(&space).contains_key("Old news"));
}

#[test]
fn a_restart_or_a_lost_cursor_adds_nothing_twice() {
    let mut store = Store::new();
    for i in 0..5 {
        store.add(
            &inbox(GMAIL),
            now(),
            &message(&format!("m{i}@x"), ADA, &format!("Note {i}"), now(), ""),
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let connector = store.connector();
    let mut cursor = None;
    let written = run(&connector, &mut space, &mut cursor, History::FromToday, &[]).unwrap();
    assert_eq!(written.items, 5);
    let before = threads(&space);
    let nodes = space.node_count().unwrap();

    // The service stopped after writing a batch but before saving the cursor, or its state
    // was lost: everything is read again, and nothing is added.
    let mut lost = None;
    let written = run(&connector, &mut space, &mut lost, History::FromToday, &[]).unwrap();
    assert_eq!(written.items, 0);
    assert_eq!(written.created, 0);
    assert_eq!(threads(&space), before);
    assert_eq!(space.node_count().unwrap(), nodes);
    assert_eq!(lost, cursor);

    // Mail rebuilt its index: the numbers start again, and still nothing is added twice.
    let rebuilt = Some(Cursor::new(r#"{"store":"V10","rowid":999999}"#));
    let mut cursor = rebuilt;
    let written = run(&connector, &mut space, &mut cursor, History::FromToday, &[]).unwrap();
    assert_eq!(written.items, 0);
    assert_eq!(space.node_count().unwrap(), nodes);
    assert!(cursor.unwrap().as_str().contains("\"rowid\":5"));
}

#[test]
fn excluded_mailboxes_spam_and_trash_are_never_read() {
    let mut store = Store::new();
    store.add(
        &inbox(GMAIL),
        now(),
        &message("g1@x", ADA, "Kept", now(), ""),
    );
    store.add(
        &inbox(ICLOUD),
        now(),
        &message("i1@x", CLEO, "Excluded", now(), ""),
    );
    store.add(
        &format!("imap://{GMAIL}/%5BGmail%5D/Spam"),
        now(),
        &message("s1@x", ADA, "Spam", now(), ""),
    );
    store.add(
        &format!("imap://{GMAIL}/%5BGmail%5D/Trash"),
        now(),
        &message("t1@x", ADA, "Trashed", now(), ""),
    );
    store.add(
        &format!("imap://{ICLOUD}/Junk"),
        now(),
        &message("j1@x", CLEO, "Junk", now(), ""),
    );
    store.add(
        &format!("imap://{ICLOUD}/Deleted%20Messages"),
        now(),
        &message("d1@x", CLEO, "Deleted", now(), ""),
    );
    let gone = store.add(
        &inbox(GMAIL),
        now(),
        &message("x1@x", ADA, "Removed", now(), ""),
    );
    store.set_deleted(gone);
    // An address Gmail sends from names the account where two accounts share a mailbox name.
    store.add(
        &format!("imap://{GMAIL}/%5BGmail%5D/Sent%20Mail"),
        long_ago(),
        &message("sent@x", ADA, "Sent long ago", long_ago(), ""),
    );

    let connector = store.connector();
    let request = FetchRequest {
        space_id: "space",
        cursor: None,
        history: History::FromToday,
        since: start_of_today(),
        excluded: &[],
        max_items: 10,
        secrets: &NoSecrets,
    };
    let containers = connector.containers(&request).unwrap();
    let names: Vec<&str> = containers.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Archive",
            "INBOX - account 7E8F9A0B",
            "INBOX - ada@gmail.example.com",
            "[Gmail]/All Mail",
            "[Gmail]/Sent Mail",
        ]
    );
    let icloud = containers
        .iter()
        .find(|c| c.id == inbox(ICLOUD))
        .unwrap()
        .clone();

    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut cursor = None;
    run(
        &connector,
        &mut space,
        &mut cursor,
        History::FromToday,
        std::slice::from_ref(&icloud),
    )
    .unwrap();
    assert_eq!(threads(&space).keys().collect::<Vec<_>>(), ["Kept"]);

    // New mail of an excluded mailbox is not read either; a new mailbox comes in.
    store.add(
        &inbox(ICLOUD),
        now(),
        &message("i2@x", CLEO, "Excluded too", now(), ""),
    );
    let projects = format!("imap://{ICLOUD}/Projects");
    store.add(
        &projects,
        now(),
        &message("p1@x", CLEO, "New mailbox", now(), ""),
    );
    run(
        &connector,
        &mut space,
        &mut cursor,
        History::FromToday,
        &[icloud],
    )
    .unwrap();
    assert_eq!(
        threads(&space).keys().collect::<Vec<_>>(),
        ["Kept", "New mailbox"]
    );
}

#[test]
fn a_message_mail_is_still_writing_comes_in_on_a_later_run() {
    let mut store = Store::new();
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let connector = store.connector();
    let mut cursor = None;
    store.add(
        &inbox(GMAIL),
        now(),
        &message("a@x", ADA, "First", now(), ""),
    );
    // Listed in the index when the copy is taken, but its file is not written yet.
    let late = store.list(&inbox(GMAIL), now(), "ada@gmail.example.com");
    store.add(
        &inbox(GMAIL),
        now(),
        &message("c@x", ADA, "Third", now(), ""),
    );
    let written = run(&connector, &mut space, &mut cursor, History::FromToday, &[]).unwrap();
    assert_eq!(written.items, 2);
    assert!(cursor.as_ref().unwrap().as_str().contains("waiting"));

    store.write(
        &inbox(GMAIL),
        late,
        &message("b@x", ADA, "Second", now(), ""),
    );
    let written = run(&connector, &mut space, &mut cursor, History::FromToday, &[]).unwrap();
    assert_eq!(written.items, 1);
    assert_eq!(
        threads(&space).keys().collect::<Vec<_>>(),
        ["First", "Second", "Third"]
    );
    assert!(!cursor.as_ref().unwrap().as_str().contains("waiting"));

    // A message written to the index after the copy was taken is read on the next run.
    let rowid = store.list(&inbox(ICLOUD), now(), "cleo@icloud.example.net");
    let written = run(&connector, &mut space, &mut cursor, History::FromToday, &[]).unwrap();
    assert_eq!(written.items, 0);
    store.write(
        &inbox(ICLOUD),
        rowid,
        &message("d@x", CLEO, "Fourth", now(), ""),
    );
    let written = run(&connector, &mut space, &mut cursor, History::FromToday, &[]).unwrap();
    assert_eq!(written.items, 1);
}

#[test]
fn replies_join_their_thread_and_edits_are_kept() {
    let mut store = Store::new();
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let connector = store.connector();
    let mut cursor = None;
    store.add(
        &inbox(GMAIL),
        now(),
        &message("root@x", ADA, "Plans", now(), ""),
    );
    run(&connector, &mut space, &mut cursor, History::FromToday, &[]).unwrap();
    let node = space.find_by_title("Plans").unwrap().remove(0);
    let mut body = space.get_node(&node.id).unwrap().unwrap().body;
    body.push_str("\nMy note about the plans.\n");
    space
        .update_node(
            &node.id,
            knode_format::NodeUpdate {
                body: Some(body),
                ..Default::default()
            },
            &Provenance::person("Example"),
        )
        .unwrap();

    // A larger number, so its file is under `Data/<digits>/Messages`.
    store
        .index
        .execute(
            "UPDATE sqlite_sequence SET seq = 12344 WHERE name = 'messages'",
            [],
        )
        .unwrap();
    let reply = store.add(
        &inbox(GMAIL),
        now(),
        &message(
            "reply@x",
            CLEO,
            "Re: Plans",
            now(),
            "In-Reply-To: <root@x>\r\nReferences: <root@x>\r\n",
        ),
    );
    assert_eq!(reply, 12345);
    assert!(
        store
            .file(&inbox(GMAIL), reply, false)
            .ends_with("Data/2/1/Messages/12345.emlx")
    );
    let written = run(&connector, &mut space, &mut cursor, History::FromToday, &[]).unwrap();
    assert_eq!(written.items, 1);
    assert_eq!(threads(&space).get("Plans"), Some(&2));
    let body = thread_body(&space, "Plans");
    assert!(body.contains("My note about the plans."));
    assert!(body.contains("Hello from reply@x."));
}

#[test]
fn partial_messages_are_read_without_their_files() {
    let mut store = Store::new();
    let rowid = store.list(&inbox(GMAIL), now(), "ada@gmail.example.com");
    let file = store.file(&inbox(GMAIL), rowid, true);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(
        &file,
        emlx(&message("partial@x", ADA, "With a file", now(), "")),
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut cursor = None;
    let written = run(
        &store.connector(),
        &mut space,
        &mut cursor,
        History::FromToday,
        &[],
    )
    .unwrap();
    assert_eq!(written.items, 1);
    assert!(
        space
            .attachments(&space.find_by_title("With a file").unwrap()[0].id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn mail_imported_before_from_takeout_or_an_export_is_not_added_again() {
    let dir = tempfile::tempdir().unwrap();
    let at = now() - 3600;
    let messages = [
        message("t1@x", ADA, "Trip", at, ""),
        message(
            "t2@x",
            CLEO,
            "Re: Trip",
            at + 60,
            "In-Reply-To: <t1@x>\r\nReferences: <t1@x>\r\n",
        ),
    ];
    // A Gmail Takeout mbox of the same two messages.
    let takeout = dir.path().join("All mail Including Spam and Trash.mbox");
    let mut mbox = String::new();
    for m in &messages {
        mbox.push_str("From 1@xxx Mon Mar 02 10:00:00 +0000 2026\nX-Gmail-Labels: Inbox\n");
        mbox.push_str(&m.replace("\r\n", "\n"));
        mbox.push('\n');
    }
    fs::write(&takeout, &mbox).unwrap();
    // A mailbox Apple Mail exported: `INBOX.mbox/mbox`.
    let export = dir.path().join("Exported").join("INBOX.mbox");
    fs::create_dir_all(&export).unwrap();
    fs::write(export.join("mbox"), &mbox).unwrap();

    for (source, path) in [(MailSource::Mbox, takeout), (MailSource::AppleMail, export)] {
        let mut store = Store::new();
        for m in &messages {
            store.add(&inbox(GMAIL), at, m);
            store.add(&all_mail(), at, m);
        }
        let room = tempfile::tempdir().unwrap();
        let mut space = space(room.path());
        let report = import_mail(
            &mut space,
            source,
            &path,
            &MailOptions::default(),
            &Provenance::person("Example"),
        )
        .unwrap();
        assert_eq!(report.added, 2, "{source:?}");
        let before = threads(&space);
        assert_eq!(before.get("Trip"), Some(&2));
        let nodes = space.node_count().unwrap();

        let mut cursor = None;
        let written = run(
            &store.connector(),
            &mut space,
            &mut cursor,
            History::Everything,
            &[],
        )
        .unwrap();
        assert_eq!(written.items, 0, "{source:?}");
        assert_eq!(written.created, 0, "{source:?}");
        assert_eq!(threads(&space), before, "{source:?}");
        assert_eq!(space.node_count().unwrap(), nodes, "{source:?}");
        assert_eq!(
            thread_body(&space, "Trip")
                .matches("Hello from t1@x.")
                .count(),
            1
        );
    }
}

#[test]
fn a_changed_index_writes_nothing_and_says_the_connector_needs_an_update() {
    // A Mail version without the `deleted` column.
    let mut store = Store::with_schema(
        "CREATE TABLE messages (ROWID INTEGER PRIMARY KEY, sender INTEGER, \
         date_received INTEGER, mailbox INTEGER);",
    );
    let mailbox = store.mailbox(&inbox(GMAIL));
    store
        .index
        .execute(
            "INSERT INTO messages (date_received, mailbox) VALUES (?1, ?2)",
            [now(), mailbox],
        )
        .unwrap();
    store.write(&inbox(GMAIL), 1, &message("a@x", ADA, "Hidden", now(), ""));
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let nodes = space.node_count().unwrap();
    let connector = store.connector();
    let err = connector.check().unwrap_err();
    assert_eq!(err.kind(), "needs-update");
    assert!(err.to_string().contains("messages.deleted"), "{err}");
    let mut cursor = None;
    let err = run(
        &connector,
        &mut space,
        &mut cursor,
        History::Everything,
        &[],
    )
    .unwrap_err();
    assert_eq!(err.kind(), "needs-update");
    assert_eq!(space.node_count().unwrap(), nodes);
    assert!(cursor.is_none());
}

#[test]
fn without_full_disk_access_it_says_how_to_allow_it() {
    let store = Store::new();
    let connector = store.connector();
    let mut perms = fs::metadata(store.mail_dir()).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o000);
    fs::set_permissions(store.mail_dir(), perms.clone()).unwrap();
    let refused = fs::read_dir(store.mail_dir()).is_err();
    let result = connector.check();
    perms.set_mode(0o755);
    fs::set_permissions(store.mail_dir(), perms).unwrap();
    if !refused {
        // Running as a user every folder is open to: nothing to test.
        return;
    }
    let err = result.unwrap_err();
    assert_eq!(err.kind(), "permission");
    assert!(matches!(
        err,
        SourceError::Permission(Permission::FullDiskAccess, _)
    ));
    let text = err.to_string();
    assert!(text.contains("Full Disk Access"), "{text}");
    assert!(
        text.contains("System Settings > Privacy & Security"),
        "{text}"
    );
}

#[test]
fn without_mail_it_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let connector = AppleMailConnector::new(dir.path().join("Library/Mail"));
    assert_eq!(connector.check().unwrap_err().kind(), "unavailable");
    fs::create_dir_all(dir.path().join("Library/Mail/V10")).unwrap();
    assert_eq!(connector.check().unwrap_err().kind(), "unavailable");
    let info = connector.info();
    assert_eq!(info.id, "apple-mail");
    assert_eq!(info.container.as_deref(), Some("mailbox"));
    assert_eq!(info.permissions, vec![Permission::FullDiskAccess]);
}

#[test]
fn the_service_brings_in_new_mail_when_mail_writes_its_index() {
    let mut store = Store::new();
    store.add(
        &inbox(GMAIL),
        long_ago(),
        &message("old@x", ADA, "Old news", long_ago(), ""),
    );
    let connector = store.connector();
    match connector.trigger() {
        Trigger::Watch { paths, every } => {
            assert!(paths[0].ends_with("V10/MailData/Envelope Index"));
            assert!(paths[1].ends_with("V10/MailData/Envelope Index-wal"));
            assert!(every <= Duration::from_secs(60));
        }
        other => panic!("{other:?}"),
    }
    let dir = tempfile::tempdir().unwrap();
    drop(space(dir.path()));
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let space_path = fs::canonicalize(dir.path().join("Personal.knode")).unwrap();
    fs::write(
        home.join("config.toml"),
        format!("default_space = {:?}\n", space_path.to_str().unwrap()),
    )
    .unwrap();
    let options = || SourcesOptions {
        settings_poll: Duration::from_millis(50),
        quiet: Duration::from_millis(100),
        ..SourcesOptions::with_connectors(vec![Arc::new(connector.clone())])
    };
    let service = start(Config::new(&home).with_sources(options())).unwrap();
    let client = Client::connect(home.join("service.sock")).unwrap();
    client
        .sources_connect(&space_path, ID, None, &Provenance::person("Example"))
        .unwrap();
    let titles = |client: &Client| -> Vec<String> {
        let mut t: Vec<String> = client
            .recent(&space_path, 100)
            .unwrap()
            .into_iter()
            .filter(|n| n.kind == kinds::THREAD)
            .map(|n| n.title)
            .collect();
        t.sort();
        t
    };
    let wait = |what: &str, done: &dyn Fn() -> bool| {
        let start = Instant::now();
        while !done() {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "timed out: {what}"
            );
            thread::sleep(Duration::from_millis(50));
        }
    };
    // The first run reads nothing older than today.
    wait("the first run", &|| {
        client
            .sources_list(&space_path)
            .unwrap()
            .into_iter()
            .any(|s| s.info.id == ID && s.last_success.is_some())
    });
    assert!(titles(&client).is_empty());
    // Give the watcher time to start, then Mail writes a new message.
    thread::sleep(Duration::from_millis(300));
    store.add(
        &inbox(ICLOUD),
        now(),
        &message("new@x", CLEO, "Just arrived", now(), ""),
    );
    store.reopen();
    wait("the new mail", &|| titles(&client) == ["Just arrived"]);
    service.stop().unwrap();

    // After a restart, nothing is added twice.
    let service = start(Config::new(&home).with_sources(options())).unwrap();
    let client = Client::connect(home.join("service.sock")).unwrap();
    // The watcher starts with the sources thread.
    thread::sleep(Duration::from_millis(500));
    store.add(
        &inbox(GMAIL),
        now(),
        &message("next@x", ADA, "Next one", now(), ""),
    );
    store.reopen();
    wait("the next mail", &|| titles(&client).len() == 2);
    thread::sleep(Duration::from_millis(300));
    assert_eq!(titles(&client), ["Just arrived", "Next one"]);
    let last_run = |client: &Client| {
        client
            .sources_list(&space_path)
            .unwrap()
            .into_iter()
            .find(|s| s.info.id == ID)
            .unwrap()
            .last_run
    };
    // Reading the copy does not wake the connector again.
    let ran = last_run(&client);
    thread::sleep(Duration::from_millis(1500));
    assert_eq!(last_run(&client), ran);
    let space = Space::open(&space_path).unwrap();
    assert_eq!(threads(&space).values().sum::<usize>(), 2);
    drop(space);
    service.stop().unwrap();
}
