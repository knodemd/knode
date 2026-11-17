//! The IMAP connector against a mail server in memory: no network, no real account.

use std::sync::{Arc, Mutex};

use chrono::NaiveDate;
use knode_format::Timestamp;
use knode_ingest::sources::sign_in::SignInValues;
use knode_ingest::sources::{Connector, Container, Cursor, History, Secrets, SourceError};

use super::client::{
    Account, FetchedMessage, ImapConnect, ImapError, ImapSession, Mailbox, MailboxStatus,
};
use super::*;
use crate::sources::mail_test_support::{
    MemSecrets, day, message, message_ids, request, space, threads, write,
};

#[derive(Clone)]
struct Stored {
    uid: u32,
    date: Timestamp,
    bytes: Vec<u8>,
    gmail_id: u64,
    labels: Vec<String>,
}

struct Box_ {
    mailbox: Mailbox,
    validity: u32,
    next: u32,
    messages: Vec<Stored>,
}

/// A mail server in memory.
#[derive(Default)]
struct Server {
    gmail: bool,
    password: String,
    boxes: Vec<Box_>,
    /// How many messages were fetched, over every session.
    fetched: usize,
    /// How many times a mailbox was examined.
    examined: usize,
}

impl Server {
    fn new(password: &str, mailboxes: &[(&str, &[&str])]) -> Self {
        Server {
            gmail: false,
            password: password.into(),
            boxes: mailboxes
                .iter()
                .map(|(name, attributes)| Box_ {
                    mailbox: Mailbox::new(*name, attributes),
                    validity: 1,
                    next: 1,
                    messages: Vec::new(),
                })
                .collect(),
            fetched: 0,
            examined: 0,
        }
    }

    fn deliver(&mut self, mailbox: &str, raw: &str, date: Timestamp, labels: &[&str]) -> u32 {
        let b = self
            .boxes
            .iter_mut()
            .find(|b| b.mailbox.name == mailbox)
            .unwrap();
        let uid = b.next;
        b.next += 1;
        b.messages.push(Stored {
            uid,
            date,
            bytes: raw.as_bytes().to_vec(),
            gmail_id: 1_000_000 + uid as u64,
            labels: labels.iter().map(|l| l.to_string()).collect(),
        });
        uid
    }

    /// The server renumbers a mailbox: a new `UIDVALIDITY` and new UIDs.
    fn renumber(&mut self, mailbox: &str) {
        let b = self
            .boxes
            .iter_mut()
            .find(|b| b.mailbox.name == mailbox)
            .unwrap();
        b.validity += 1;
        let mut uid = 100;
        for m in &mut b.messages {
            m.uid = uid;
            uid += 1;
        }
        b.next = uid;
    }
}

#[derive(Clone, Default)]
struct Net(Arc<Mutex<Server>>);

struct MemSession {
    server: Arc<Mutex<Server>>,
    selected: Option<String>,
}

impl ImapConnect for Net {
    fn connect(
        &self,
        account: &Account,
        password: &str,
    ) -> Result<Box<dyn ImapSession>, ImapError> {
        if account.host == "down.example.com" {
            return Err(ImapError::Network("connection refused".into()));
        }
        if self.0.lock().unwrap().password != password {
            return Err(ImapError::Login(
                "[AUTHENTICATIONFAILED] Invalid credentials".into(),
            ));
        }
        Ok(Box::new(MemSession {
            server: self.0.clone(),
            selected: None,
        }))
    }
}

impl ImapSession for MemSession {
    fn capabilities(&mut self) -> Result<Vec<String>, ImapError> {
        let mut caps = vec!["IMAP4REV1".to_string()];
        if self.server.lock().unwrap().gmail {
            caps.push("X-GM-EXT-1".into());
        }
        Ok(caps)
    }

    fn list(&mut self) -> Result<Vec<Mailbox>, ImapError> {
        Ok(self
            .server
            .lock()
            .unwrap()
            .boxes
            .iter()
            .map(|b| b.mailbox.clone())
            .collect())
    }

    fn status(&mut self, mailbox: &str) -> Result<MailboxStatus, ImapError> {
        let server = self.server.lock().unwrap();
        let b = server
            .boxes
            .iter()
            .find(|b| b.mailbox.name == mailbox)
            .ok_or_else(|| ImapError::Protocol("NO no such mailbox".into()))?;
        Ok(MailboxStatus {
            uid_validity: b.validity,
            uid_next: Some(b.next),
        })
    }

    fn examine(&mut self, mailbox: &str) -> Result<MailboxStatus, ImapError> {
        let status = self.status(mailbox)?;
        self.server.lock().unwrap().examined += 1;
        self.selected = Some(mailbox.to_string());
        Ok(status)
    }

    fn search(&mut self, from_uid: u32, since: Option<NaiveDate>) -> Result<Vec<u32>, ImapError> {
        let server = self.server.lock().unwrap();
        let name = self.selected.clone().expect("a mailbox is selected");
        let b = server
            .boxes
            .iter()
            .find(|b| b.mailbox.name == name)
            .unwrap();
        let mut uids: Vec<u32> = b
            .messages
            .iter()
            .filter(|m| m.uid >= from_uid)
            .filter(|m| {
                since.is_none_or(|d| {
                    chrono::DateTime::from_timestamp_millis(m.date.0)
                        .unwrap()
                        .date_naive()
                        >= d
                })
            })
            .map(|m| m.uid)
            .collect();
        // Like real servers, `n:*` always includes the last message.
        if uids.is_empty()
            && let Some(last) = b.messages.iter().map(|m| m.uid).max()
        {
            uids.push(last);
        }
        Ok(uids)
    }

    fn fetch(&mut self, uids: &[u32], gmail: bool) -> Result<Vec<FetchedMessage>, ImapError> {
        let mut server = self.server.lock().unwrap();
        let name = self.selected.clone().expect("a mailbox is selected");
        let b = server
            .boxes
            .iter()
            .find(|b| b.mailbox.name == name)
            .unwrap();
        let out: Vec<FetchedMessage> = b
            .messages
            .iter()
            .filter(|m| uids.contains(&m.uid))
            .map(|m| FetchedMessage {
                uid: m.uid,
                internal_date: Some(m.date),
                bytes: Some(m.bytes.clone()),
                gmail_id: gmail.then_some(m.gmail_id),
                gmail_labels: if gmail { m.labels.clone() } else { Vec::new() },
            })
            .collect();
        server.fetched += out.len();
        Ok(out)
    }

    fn logout(&mut self) {}
}

fn signed_in(net: &Net, host: &str) -> (ImapConnector, MemSecrets) {
    let connector = ImapConnector::with_network(Arc::new(net.clone()));
    let secrets = MemSecrets::default();
    let values = SignInValues::default()
        .with("username", "ana@example.com")
        .with("password", "app-password")
        .with("server", host);
    assert_eq!(
        connector.answer("account", &values, &secrets).unwrap(),
        None
    );
    (connector, secrets)
}

fn generic() -> Net {
    let server = Server::new(
        "app-password",
        &[
            ("INBOX", &[]),
            ("Archive", &["\\Archive"]),
            ("Junk", &["\\Junk"]),
            ("Deleted Messages", &[]),
            ("Family", &[]),
            ("Lists", &["\\Noselect"]),
        ],
    );
    Net(Arc::new(Mutex::new(server)))
}

/// Runs the connector until it has nothing more, writing each batch; returns the cursor and
/// how many messages were new.
fn run(
    connector: &ImapConnector,
    space: &mut knode_format::Space,
    cursor: Option<Cursor>,
    history: History,
    excluded: &[Container],
    secrets: &dyn Secrets,
) -> (Cursor, usize) {
    let mut cursor = cursor;
    let mut added = 0;
    loop {
        let fetched = connector
            .fetch(&request(cursor.as_ref(), history, excluded, secrets))
            .unwrap();
        let more = fetched.more;
        cursor = Some(fetched.cursor.clone());
        added += write(space, fetched, ID);
        if !more {
            return (cursor.unwrap(), added);
        }
    }
}

#[test]
fn signs_in_with_an_app_password_and_keeps_it_in_the_secrets() {
    let net = generic();
    let connector = ImapConnector::with_network(Arc::new(net.clone()));
    let secrets = MemSecrets::default();
    let step = connector.start(&secrets).unwrap().unwrap();
    assert_eq!(step.id, "account");
    assert!(step.fields.iter().any(|f| f.id == "password" && f.secret));

    let wrong = SignInValues::default()
        .with("username", "ana@example.com")
        .with("password", "nope")
        .with("server", "imap.example.com");
    let error = connector.answer("account", &wrong, &secrets).unwrap_err();
    assert!(matches!(error, SourceError::SignIn(_)), "{error}");
    assert!(secrets.names().is_empty());

    let plain = SignInValues::default()
        .with("username", "ana@example.com")
        .with("password", "app-password")
        .with("server", "imap.example.com:143");
    assert!(connector.answer("account", &plain, &secrets).is_err());

    let (connector, secrets) = signed_in(&net, "imap.example.com");
    assert_eq!(secrets.names(), vec!["account", "password"]);
    assert_eq!(connector.start(&secrets).unwrap(), None);
    assert_eq!(
        SignIn::account(&connector, &secrets).unwrap().as_deref(),
        Some("ana@example.com")
    );
    // The account holds no password.
    assert!(
        !secrets
            .get("account")
            .unwrap()
            .unwrap()
            .contains("app-password")
    );
    assert!(connector.sign_out(&secrets).unwrap());
    assert!(secrets.names().is_empty());
    assert!(!connector.sign_out(&secrets).unwrap());
}

#[test]
fn gmail_addresses_need_no_server_and_spaces_in_app_passwords_are_dropped() {
    let net = generic();
    let connector = ImapConnector::with_network(Arc::new(net.clone()));
    let secrets = MemSecrets::default();
    net.0.lock().unwrap().password = "abcdefghijklmnop".into();
    let values = SignInValues::default()
        .with("username", "ana@gmail.com")
        .with("password", "abcd efgh ijkl mnop");
    assert_eq!(
        connector.answer("account", &values, &secrets).unwrap(),
        None
    );
    let account: Account = serde_json::from_str(&secrets.get("account").unwrap().unwrap()).unwrap();
    assert_eq!(account.host, "imap.gmail.com");
    assert_eq!(account.port, 993);
}

#[test]
fn without_signing_in_it_asks_to_sign_in() {
    let connector = ImapConnector::with_network(Arc::new(generic()));
    let secrets = MemSecrets::default();
    let error = connector
        .fetch(&request(None, History::Everything, &[], &secrets))
        .unwrap_err();
    assert_eq!(error.kind(), "sign-in");
}

#[test]
fn a_changed_password_asks_to_sign_in_again_and_a_down_server_is_retried() {
    let net = generic();
    let (connector, secrets) = signed_in(&net, "imap.example.com");
    net.0.lock().unwrap().password = "revoked".into();
    let error = connector
        .fetch(&request(None, History::Everything, &[], &secrets))
        .unwrap_err();
    assert_eq!(error.kind(), "sign-in");

    let account = Account {
        host: "down.example.com".into(),
        port: 993,
        username: "ana".into(),
    };
    secrets
        .set("account", &serde_json::to_string(&account).unwrap())
        .unwrap();
    let error = connector
        .fetch(&request(None, History::Everything, &[], &secrets))
        .unwrap_err();
    assert_eq!(error.kind(), "failed");
}

#[test]
fn lists_mailboxes_without_spam_trash_or_folders_that_hold_no_mail() {
    let net = generic();
    let (connector, secrets) = signed_in(&net, "imap.example.com");
    let containers = connector
        .containers(&request(None, History::Everything, &[], &secrets))
        .unwrap();
    let ids: Vec<&str> = containers.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, vec!["INBOX", "Archive", "Family"]);
}

#[test]
fn brings_in_new_mail_once_and_a_restart_adds_nothing_twice() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let net = generic();
    {
        let mut server = net.0.lock().unwrap();
        server.deliver(
            "INBOX",
            &message("m1@example.com", "Plans", 11, "one"),
            day(11),
            &[],
        );
        server.deliver(
            "Archive",
            &message("m2@example.com", "Old", 3, "two"),
            day(3),
            &[],
        );
        server.deliver(
            "Junk",
            &message("spam@example.com", "Win", 12, "spam"),
            day(12),
            &[],
        );
        server.deliver(
            "Deleted Messages",
            &message("gone@example.com", "Bin", 12, "gone"),
            day(12),
            &[],
        );
    }
    let (connector, secrets) = signed_in(&net, "imap.example.com");
    let (cursor, added) = run(
        &connector,
        &mut space,
        None,
        History::Everything,
        &[],
        &secrets,
    );
    assert_eq!(added, 2);
    assert_eq!(
        message_ids(&space),
        vec!["m1@example.com", "m2@example.com"]
    );

    // Nothing new: no mailbox is even opened.
    let examined = net.0.lock().unwrap().examined;
    let (cursor, added) = run(
        &connector,
        &mut space,
        Some(cursor),
        History::Everything,
        &[],
        &secrets,
    );
    assert_eq!(added, 0);
    assert_eq!(net.0.lock().unwrap().examined, examined);

    // A reply arrives: it joins the thread.
    let reply = message("m3@example.com", "Re: Plans", 12, "three")
        .replace("Subject:", "In-Reply-To: <m1@example.com>\r\nSubject:");
    net.0.lock().unwrap().deliver("INBOX", &reply, day(12), &[]);
    let fetched_before = net.0.lock().unwrap().fetched;
    let (cursor, added) = run(
        &connector,
        &mut space,
        Some(cursor.clone()),
        History::Everything,
        &[],
        &secrets,
    );
    assert_eq!(added, 1);
    assert_eq!(net.0.lock().unwrap().fetched, fetched_before + 1);
    assert_eq!(threads(&space).len(), 2);

    // The service stopped after writing but before saving the cursor: the same messages are
    // read again and nothing is added twice.
    let (_, added) = run(
        &connector,
        &mut space,
        None,
        History::Everything,
        &[],
        &secrets,
    );
    assert_eq!(added, 0);
    assert_eq!(message_ids(&space).len(), 3);
    let _ = cursor;
}

#[test]
fn a_new_uidvalidity_reads_the_mailbox_again_without_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let net = generic();
    net.0.lock().unwrap().deliver(
        "INBOX",
        &message("m1@example.com", "Plans", 11, "one"),
        day(11),
        &[],
    );
    let (connector, secrets) = signed_in(&net, "imap.example.com");
    let (cursor, _) = run(
        &connector,
        &mut space,
        None,
        History::Everything,
        &[],
        &secrets,
    );

    net.0.lock().unwrap().renumber("INBOX");
    net.0.lock().unwrap().deliver(
        "INBOX",
        &message("m4@example.com", "News", 12, "four"),
        day(12),
        &[],
    );
    let fetched_before = net.0.lock().unwrap().fetched;
    let (cursor, added) = run(
        &connector,
        &mut space,
        Some(cursor),
        History::Everything,
        &[],
        &secrets,
    );
    assert_eq!(added, 1);
    // Both were read again under the new UIDVALIDITY.
    assert_eq!(net.0.lock().unwrap().fetched, fetched_before + 2);
    assert_eq!(
        message_ids(&space),
        vec!["m1@example.com", "m4@example.com"]
    );
    let saved: serde_json::Value = serde_json::from_str(cursor.as_str()).unwrap();
    assert_eq!(saved["mailboxes"]["INBOX"]["validity"], 2);
}

#[test]
fn from_today_leaves_out_older_mail_and_excluded_mailboxes_are_never_opened() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let net = generic();
    {
        let mut server = net.0.lock().unwrap();
        server.deliver(
            "INBOX",
            &message("old@example.com", "Old", 2, "old"),
            day(2),
            &[],
        );
        server.deliver(
            "INBOX",
            &message("new@example.com", "New", 11, "new"),
            day(11),
            &[],
        );
        server.deliver(
            "Family",
            &message("fam@example.com", "Family", 11, "f"),
            day(11),
            &[],
        );
    }
    let (connector, secrets) = signed_in(&net, "imap.example.com");
    let excluded = [Container::new("Family", "Family")];
    let (_, added) = run(
        &connector,
        &mut space,
        None,
        History::FromToday,
        &excluded,
        &secrets,
    );
    assert_eq!(added, 1);
    assert_eq!(message_ids(&space), vec!["new@example.com"]);
    // INBOX and Archive were opened, never Family.
    assert_eq!(net.0.lock().unwrap().examined, 2);
}

#[test]
fn large_mailboxes_come_in_batches() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let net = generic();
    for i in 0..30 {
        net.0.lock().unwrap().deliver(
            "INBOX",
            &message(
                &format!("b{i:02}@example.com"),
                &format!("Batch {i}"),
                11,
                "x",
            ),
            day(11),
            &[],
        );
    }
    let (connector, secrets) = signed_in(&net, "imap.example.com");
    let mut req = request(None, History::Everything, &[], &secrets);
    req.max_items = 10;
    let first = connector.fetch(&req).unwrap();
    assert!(first.more);
    assert_eq!(first.batch.as_ref().unwrap().len(), 10);
    let cursor = first.cursor.clone();
    write(&mut space, first, ID);
    let (_, added) = run(
        &connector,
        &mut space,
        Some(cursor),
        History::Everything,
        &[],
        &secrets,
    );
    assert_eq!(added, 20);
    assert_eq!(message_ids(&space).len(), 30);
}

#[test]
fn another_account_starts_again() {
    let net = generic();
    net.0.lock().unwrap().deliver(
        "INBOX",
        &message("m1@example.com", "Plans", 11, "one"),
        day(11),
        &[],
    );
    let (connector, secrets) = signed_in(&net, "imap.example.com");
    let cursor = Cursor::new(
        r#"{"account":"someone@else:993","mailboxes":{"INBOX":{"validity":1,"last":99,"next":100}}}"#,
    );
    let fetched = connector
        .fetch(&request(Some(&cursor), History::Everything, &[], &secrets))
        .unwrap();
    assert_eq!(fetched.batch.unwrap().len(), 1);
}

fn gmail() -> Net {
    let mut server = Server::new(
        "app-password",
        &[
            ("INBOX", &[]),
            ("[Gmail]", &["\\Noselect", "\\HasChildren"]),
            ("[Gmail]/All Mail", &["\\All"]),
            ("[Gmail]/Sent Mail", &["\\Sent"]),
            ("[Gmail]/Spam", &["\\Junk"]),
            ("[Gmail]/Trash", &["\\Trash"]),
            ("[Gmail]/Starred", &["\\Flagged"]),
            ("Work", &[]),
            ("Private", &[]),
        ],
    );
    server.gmail = true;
    Net(Arc::new(Mutex::new(server)))
}

#[test]
fn on_gmail_reads_all_mail_once_with_labels_and_excluded_labels_stay_out() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let net = gmail();
    {
        let mut server = net.0.lock().unwrap();
        server.deliver(
            "[Gmail]/All Mail",
            &message("g1@example.com", "Work plans", 11, "w"),
            day(11),
            &["\\Inbox", "\\Important", "Work"],
        );
        server.deliver(
            "[Gmail]/All Mail",
            &message("g2@example.com", "Secret", 11, "p"),
            day(11),
            &["\\Inbox", "Private"],
        );
        server.deliver(
            "[Gmail]/All Mail",
            &message("g3@example.com", "Archived", 11, "a"),
            day(11),
            &[],
        );
        // The same message as a label mailbox: never read there.
        server.deliver(
            "INBOX",
            &message("g1@example.com", "Work plans", 11, "w"),
            day(11),
            &[],
        );
    }
    let (connector, secrets) = signed_in(&net, "imap.gmail.com");
    let containers = connector
        .containers(&request(None, History::Everything, &[], &secrets))
        .unwrap();
    let ids: Vec<&str> = containers.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "INBOX",
            "Private",
            "Work",
            "[Gmail]/Sent Mail",
            "[Gmail]/Starred"
        ]
    );

    let excluded = [Container::new("Private", "Private")];
    let (_, added) = run(
        &connector,
        &mut space,
        None,
        History::Everything,
        &excluded,
        &secrets,
    );
    assert_eq!(added, 2);
    assert_eq!(
        message_ids(&space),
        vec!["g1@example.com", "g3@example.com"]
    );
    assert_eq!(net.0.lock().unwrap().examined, 1);
    let work = threads(&space)
        .into_iter()
        .find(|t| t.title == "Work plans")
        .unwrap();
    let props = space.properties(&work.id).unwrap();
    let folders = &props["mail.folders"];
    assert!(folders.contains("Inbox") && folders.contains("Work") && folders.contains("Important"));
    assert_eq!(props["mail.source"], "imap");
}
