//! The mail connectors through Knode Service: signing the IMAP connector in over
//! `sources.sign_in`, connecting it, new mail coming in, and disconnecting forgetting the
//! account. The mail server is in memory and every address is invented; secrets are kept in
//! files of the test's Knode home, never in the real keychain.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use knode_format::{Provenance, Space};
use knode_service::sources::imap::ImapConnector;
use knode_service::sources::imap::client::{
    Account, FetchedMessage, ImapConnect, ImapError, ImapSession, Mailbox, MailboxStatus,
};
use knode_service::sources::{SignInState, SignInStep, SourcesOptions};
use knode_service::{Client, Config, ServiceHandle, start};
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(10);

/// One mailbox, INBOX, in memory.
#[derive(Default)]
struct Inbox {
    messages: Vec<Vec<u8>>,
}

#[derive(Clone, Default)]
struct Server(Arc<Mutex<Inbox>>);

struct Session(Arc<Mutex<Inbox>>);

impl ImapConnect for Server {
    fn connect(
        &self,
        _account: &Account,
        password: &str,
    ) -> Result<Box<dyn ImapSession>, ImapError> {
        if password != "app-password" {
            return Err(ImapError::Login("Invalid credentials".into()));
        }
        Ok(Box::new(Session(self.0.clone())))
    }
}

impl ImapSession for Session {
    fn capabilities(&mut self) -> Result<Vec<String>, ImapError> {
        Ok(vec!["IMAP4REV1".into()])
    }
    fn list(&mut self) -> Result<Vec<Mailbox>, ImapError> {
        Ok(vec![Mailbox::new("INBOX", &[])])
    }
    fn status(&mut self, _mailbox: &str) -> Result<MailboxStatus, ImapError> {
        Ok(MailboxStatus {
            uid_validity: 1,
            uid_next: Some(self.0.lock().unwrap().messages.len() as u32 + 1),
        })
    }
    fn examine(&mut self, mailbox: &str) -> Result<MailboxStatus, ImapError> {
        self.status(mailbox)
    }
    fn search(&mut self, from: u32, _since: Option<NaiveDate>) -> Result<Vec<u32>, ImapError> {
        let n = self.0.lock().unwrap().messages.len() as u32;
        Ok((from.max(1)..=n).collect())
    }
    fn fetch(&mut self, uids: &[u32], _gmail: bool) -> Result<Vec<FetchedMessage>, ImapError> {
        let inbox = self.0.lock().unwrap();
        Ok(uids
            .iter()
            .filter_map(|uid| {
                let bytes = inbox.messages.get(*uid as usize - 1)?.clone();
                Some(FetchedMessage {
                    uid: *uid,
                    internal_date: None,
                    bytes: Some(bytes),
                    gmail_id: None,
                    gmail_labels: Vec::new(),
                })
            })
            .collect())
    }
    fn logout(&mut self) {}
}

fn mail(id: &str, subject: &str) -> Vec<u8> {
    format!(
        "Message-ID: <{id}>\r\nFrom: Ana Example <ana@example.com>\r\nTo: bo@example.org\r\n\
         Subject: {subject}\r\nDate: Wed, 11 Nov 2026 10:00:00 +0000\r\n\r\nHello\r\n"
    )
    .into_bytes()
}

struct Fixture {
    dir: TempDir,
    server: Server,
    service: Option<ServiceHandle>,
}

impl Fixture {
    fn new() -> Self {
        // Secrets go to files in the test's Knode home, not the keychain of this machine.
        // SAFETY: set before any thread of this test binary reads the environment.
        unsafe { std::env::set_var("KNODE_KEY_STORE", "file") };
        let dir = tempfile::tempdir().unwrap();
        drop(Space::create(dir.path().join("Personal.knode"), "Personal").unwrap());
        let home = dir.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let space = fs::canonicalize(dir.path().join("Personal.knode")).unwrap();
        fs::write(
            home.join("config.toml"),
            format!("default_space = {:?}\n", space.to_str().unwrap()),
        )
        .unwrap();
        let server = Server::default();
        let options = SourcesOptions {
            settings_poll: Duration::from_millis(50),
            backoff: Duration::from_millis(200),
            max_backoff: Duration::from_millis(800),
            ..SourcesOptions::with_connectors(vec![Arc::new(ImapConnector::with_network(
                Arc::new(server.clone()),
            ))])
        };
        let service = start(Config::new(&home).with_sources(options)).unwrap();
        Fixture {
            dir,
            server,
            service: Some(service),
        }
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

    fn threads(&self) -> Vec<String> {
        let mut titles: Vec<String> = self
            .client()
            .recent(self.space(), 1000)
            .unwrap()
            .into_iter()
            .filter(|n| n.kind == "email")
            .map(|n| n.title)
            .collect();
        titles.sort();
        titles
    }

    fn wait_until(&self, what: &str, done: impl Fn(&Fixture) -> bool) {
        let start = Instant::now();
        while !done(self) {
            assert!(start.elapsed() < WAIT, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(service) = self.service.take() {
            let _ = service.stop();
        }
    }
}

#[test]
fn sign_in_connect_bring_in_and_disconnect_forgets_the_account() {
    let fixture = Fixture::new();
    let client = fixture.client();
    let me = Provenance::person("Ana");

    // Not signed in: the account, then its app password.
    assert_eq!(
        client.sources_account("imap").unwrap(),
        SignInState::SignedOut
    );
    let state = client
        .sources_sign_in(
            "imap",
            SignInStep::Start {
                account: "ana@example.com imap.example.com".into(),
            },
        )
        .unwrap();
    assert!(matches!(state, SignInState::PasswordNeeded { .. }));

    // A wrong app password is refused and nothing is kept.
    let error = client
        .sources_sign_in(
            "imap",
            SignInStep::Password {
                password: "wrong".into(),
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("refused"), "{error}");

    let done = client
        .sources_sign_in(
            "imap",
            SignInStep::Password {
                password: "app-password".into(),
            },
        )
        .unwrap();
    assert_eq!(
        done,
        SignInState::SignedIn {
            account: "ana@example.com".into()
        }
    );

    // The password is in the secrets store, never in the space or the state.
    let space_files: Vec<_> = walk(&fixture.space());
    for file in space_files {
        let text = fs::read(&file).unwrap_or_default();
        assert!(
            !String::from_utf8_lossy(&text).contains("app-password"),
            "{} holds the password",
            file.display()
        );
    }

    fixture
        .server
        .0
        .lock()
        .unwrap()
        .messages
        .push(mail("m1@example.com", "Plans"));
    client
        .sources_connect(
            fixture.space(),
            "imap",
            Some(knode_service::sources::History::Everything),
            &me,
        )
        .unwrap();
    fixture.wait_until("the first mail", |f| f.threads() == vec!["Plans"]);

    // Disconnecting it from the last space forgets the account.
    client
        .sources_disconnect(fixture.space(), "imap", &me)
        .unwrap();
    assert_eq!(
        client.sources_account("imap").unwrap(),
        SignInState::SignedOut
    );
    assert!(!client.sources_sign_out("imap").unwrap());
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}
