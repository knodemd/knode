//! Names from Contacts in the service: the Mac app gives them with `sources.contact_names`, and
//! the Messages connector names people with them in what it brings in, over a synthetic
//! Messages database with invented people. Contacts is never read here.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use knode_format::{Provenance, Space};
use knode_ingest::imessage::IMessageConnector;
use knode_service::keys::{fingerprint, generate_key};
use knode_service::protocol::codes;
use knode_service::sources::{ContactBook, History, SourcesOptions};
use knode_service::{Client, Config, Error, ServiceHandle, start};
use rusqlite::{Connection, params};
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(10);
const ADA: &str = "+15550100123";
const GRACE: &str = "grace@example.com";
const ADA_CHAT: &str = "iMessage;-;+15550100123";
const GRACE_CHAT: &str = "iMessage;-;grace@example.com";

/// The tables and columns of the Messages database the connector reads.
const SCHEMA: &str = "
CREATE TABLE message (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, guid TEXT UNIQUE NOT NULL,
  text TEXT, handle_id INTEGER DEFAULT 0, attributedBody BLOB, service TEXT, date INTEGER,
  is_from_me INTEGER DEFAULT 0, cache_has_attachments INTEGER DEFAULT 0,
  item_type INTEGER DEFAULT 0, associated_message_guid TEXT,
  associated_message_type INTEGER DEFAULT 0, balloon_bundle_id TEXT,
  thread_originator_guid TEXT, date_retracted INTEGER DEFAULT 0, date_edited INTEGER DEFAULT 0,
  message_summary_info BLOB);
CREATE TABLE chat (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, guid TEXT UNIQUE NOT NULL,
  style INTEGER, chat_identifier TEXT, service_name TEXT, display_name TEXT);
CREATE TABLE handle (ROWID INTEGER PRIMARY KEY AUTOINCREMENT UNIQUE, id TEXT NOT NULL,
  service TEXT NOT NULL, UNIQUE (id, service));
CREATE TABLE chat_message_join (chat_id INTEGER, message_id INTEGER,
  message_date INTEGER DEFAULT 0, PRIMARY KEY (chat_id, message_id));
CREATE TABLE chat_handle_join (chat_id INTEGER, handle_id INTEGER, UNIQUE(chat_id, handle_id));
CREATE TABLE attachment (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, guid TEXT UNIQUE NOT NULL,
  filename TEXT, mime_type TEXT, transfer_name TEXT, total_bytes INTEGER DEFAULT 0);
CREATE TABLE message_attachment_join (message_id INTEGER, attachment_id INTEGER,
  UNIQUE(message_id, attachment_id));
";

struct Fixture {
    dir: TempDir,
    messages: PathBuf,
    contacts: Arc<ContactBook>,
    service: Option<ServiceHandle>,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        drop(Space::create(dir.path().join("Personal.knode"), "Personal").unwrap());
        let messages = dir.path().join("Messages");
        fs::create_dir_all(messages.join("Attachments")).unwrap();
        let conn = Connection::open(messages.join("chat.db")).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        for (handle, guid) in [(ADA, ADA_CHAT), (GRACE, GRACE_CHAT)] {
            conn.execute(
                "INSERT INTO handle (id, service) VALUES (?1, 'iMessage')",
                [handle],
            )
            .unwrap();
            let h = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO chat (guid, style, chat_identifier, service_name, display_name) \
                 VALUES (?1, 45, ?2, 'iMessage', '')",
                params![guid, handle],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (?1, ?2)",
                params![conn.last_insert_rowid(), h],
            )
            .unwrap();
        }
        fs::create_dir_all(dir.path().join("home")).unwrap();
        let space = fs::canonicalize(dir.path().join("Personal.knode")).unwrap();
        fs::write(
            dir.path().join("home/config.toml"),
            format!("default_space = {:?}\n", space.to_str().unwrap()),
        )
        .unwrap();
        let mut f = Fixture {
            dir,
            messages,
            contacts: Arc::new(ContactBook::new()),
            service: None,
        };
        f.start();
        f
    }

    fn start(&mut self) {
        self.contacts = Arc::new(ContactBook::new());
        let connector = IMessageConnector::new(&self.messages).with_names(self.contacts.clone());
        let options = SourcesOptions {
            settings_poll: Duration::from_millis(50),
            quiet: Duration::from_millis(50),
            contacts: self.contacts.clone(),
            ..SourcesOptions::with_connectors(vec![Arc::new(connector)])
        };
        let config = Config::new(self.home()).with_sources(options);
        self.service = Some(start(config).unwrap());
    }

    fn restart(&mut self) {
        self.service.take().unwrap().stop().unwrap();
        self.start();
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn space(&self) -> PathBuf {
        fs::canonicalize(self.dir.path().join("Personal.knode")).unwrap()
    }

    fn client(&self) -> Client {
        Client::connect(self.home().join("service.sock")).unwrap()
    }

    /// A message from `from` in `chat`, sent now.
    fn message(&self, chat: &str, from: &str, text: &str) {
        let conn = Connection::open(self.messages.join("chat.db")).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM message", [], |r| r.get(0))
            .unwrap();
        let handle: i64 = conn
            .query_row("SELECT ROWID FROM handle WHERE id = ?1", [from], |r| {
                r.get(0)
            })
            .unwrap();
        let date = (chrono::Utc::now().timestamp() - 978_307_200) * 1_000_000_000;
        conn.execute(
            "INSERT INTO message (guid, text, handle_id, date, service) \
             VALUES (?1, ?2, ?3, ?4, 'iMessage')",
            params![
                format!("00000000-0000-4000-8000-{:012}", n + 1),
                text,
                handle,
                date
            ],
        )
        .unwrap();
        let message = conn.last_insert_rowid();
        let chat: i64 = conn
            .query_row("SELECT ROWID FROM chat WHERE guid = ?1", [chat], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute(
            "INSERT INTO chat_message_join (chat_id, message_id, message_date) \
             VALUES (?1, ?2, ?3)",
            params![chat, message, date],
        )
        .unwrap();
    }

    fn connect(&self) {
        self.client()
            .sources_connect(
                self.space(),
                "imessage",
                Some(History::Everything),
                &Provenance::person("Example"),
            )
            .unwrap();
    }

    /// Runs the connector until a chat node with this title is in the space, and returns its
    /// body.
    fn wait_for(&self, title: &str) -> String {
        let start = Instant::now();
        loop {
            let _ = self.client().sources_run(self.space(), "imessage");
            let found = self
                .client()
                .recent(self.space(), 100)
                .unwrap()
                .into_iter()
                .find(|n| n.title == title);
            if let Some(node) = found {
                let node = self.client().get_node(self.space(), &node.id).unwrap();
                return node.unwrap().body;
            }
            assert!(start.elapsed() < WAIT, "timed out waiting for {title:?}");
            thread::sleep(Duration::from_millis(100));
        }
    }
}

fn names(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(h, n)| (h.to_string(), n.to_string()))
        .collect()
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn names_from_the_app_appear_in_new_chat_nodes() {
    let f = Fixture::new();
    let kept = f
        .client()
        .sources_contact_names(names(&[
            ("(555) 010-0123", "Ada Lovelace"),
            ("Grace@Example.com", "Grace Hopper"),
        ]))
        .unwrap();
    assert_eq!(kept, 2);
    f.message(ADA_CHAT, ADA, "Hi");
    f.connect();
    let body = f.wait_for("iMessage - Ada Lovelace");
    assert!(body.contains("**Ada Lovelace**"), "{body}");
    // Kept in the Knode home, only for its owner, never in the space.
    let file = f.home().join("sources/contact-names.json");
    assert_eq!(mode(&file), 0o600);
    let space_files = walk(&f.space());
    assert!(
        space_files.iter().all(|p| !fs::read(p)
            .unwrap_or_default()
            .windows(12)
            .any(|w| w == b"Grace Hopper")),
        "a name not used yet was written to the space"
    );
}

#[test]
fn a_new_map_replaces_the_old_one_and_survives_a_restart() {
    let mut f = Fixture::new();
    f.client()
        .sources_contact_names(names(&[(ADA, "Ada Lovelace"), (GRACE, "Grace Hopper")]))
        .unwrap();
    f.client()
        .sources_contact_names(names(&[(ADA, "Ada King")]))
        .unwrap();
    f.restart();
    f.client().status().unwrap();
    assert_eq!(f.contacts.len(), 1);
    f.message(ADA_CHAT, ADA, "Hi");
    f.message(GRACE_CHAT, GRACE, "Hello");
    f.connect();
    f.wait_for("iMessage - Ada King");
    f.wait_for("iMessage - grace@example.com");
    // An empty map forgets every name and the file.
    assert_eq!(
        f.client().sources_contact_names(BTreeMap::new()).unwrap(),
        0
    );
    assert!(!f.home().join("sources/contact-names.json").exists());
    assert!(f.contacts.is_empty());
}

#[test]
fn only_a_person_gives_names() {
    let f = Fixture::new();
    let key = generate_key().unwrap();
    f.client()
        .add_agent(
            "reader",
            "claude-code",
            &fingerprint(&key),
            &Provenance::person("Example"),
        )
        .unwrap();
    let agent = Client::connect_as_agent(f.home().join("service.sock"), &key).unwrap();
    match agent.sources_contact_names(names(&[(ADA, "Mallory")])) {
        Err(Error::Rpc(e)) => assert_eq!(e.code, codes::FORBIDDEN),
        other => panic!("expected the service to refuse, got {other:?}"),
    }
    assert!(f.contacts.is_empty());
    match f
        .client()
        .call("sources.contact_names", serde_json::json!({}))
    {
        Err(Error::Rpc(e)) => assert_eq!(e.code, codes::INVALID_PARAMS),
        other => panic!("expected names to be needed, got {other:?}"),
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}
