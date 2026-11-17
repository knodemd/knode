//! `import.run` for mail: an `.mbox` file and an Apple Mail store, synthetic and written here,
//! imported through a real service. Every person and address is invented.

use std::fs;
use std::path::{Path, PathBuf};

use knode_format::{Provenance, Space};
use knode_service::import::{ImportSource, prepare};
use knode_service::protocol::codes;
use knode_service::{Client, Config, Error, ImportOutcome, ServiceHandle, start};
use knode_vault::mail::{MailOptions, kinds};
use tempfile::TempDir;

fn me() -> Provenance {
    Provenance::person("Ada")
}

struct Fixture {
    dir: TempDir,
    _service: ServiceHandle,
    person: Client,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let service = start(Config::new(dir.path().join("home"))).unwrap();
        let person = Client::connect(service.socket_path()).unwrap();
        let fixture = Fixture {
            dir,
            _service: service,
            person,
        };
        fixture
            .person
            .create_space(fixture.space(), "Work")
            .unwrap();
        fixture
    }

    fn root(&self) -> PathBuf {
        fs::canonicalize(self.dir.path()).unwrap()
    }

    fn space(&self) -> PathBuf {
        self.root().join("Work.knode")
    }
}

fn message(id: &str, labels: &str, subject: &str, date: &str, extra: &str) -> String {
    format!(
        "From 1@xxx Mon Mar 02 10:00:00 +0000 2026\nX-Gmail-Labels: {labels}\nMessage-ID: <{id}>\n\
From: Ada Example <ada@example.com>\nTo: Bruno Sample <bruno@example.org>\nSubject: {subject}\n\
Date: {date}\n{extra}\nHello from {id}.\n\n"
    )
}

/// A Takeout mbox: a thread of two messages and one other message.
fn takeout(dir: &Path) -> PathBuf {
    let mail = dir.join("Takeout").join("Mail");
    fs::create_dir_all(&mail).unwrap();
    let file = mail.join("All mail Including Spam and Trash.mbox");
    let mut text = message(
        "plan-1@example.com",
        "Inbox,Work",
        "Quarter plan",
        "Mon, 2 Mar 2026 09:00:00 +0000",
        "",
    );
    text.push_str(&message(
        "plan-2@example.org",
        "Inbox,Work",
        "Re: Quarter plan",
        "Tue, 3 Mar 2026 09:00:00 +0000",
        "In-Reply-To: <plan-1@example.com>\n",
    ));
    text.push_str(&message(
        "lunch@example.com",
        "Inbox",
        "Lunch",
        "Wed, 1 Apr 2026 12:00:00 +0000",
        "",
    ));
    fs::write(&file, text).unwrap();
    file
}

fn emlx(path: &Path, message: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = format!("{}\n", message.len()).into_bytes();
    bytes.extend_from_slice(message.as_bytes());
    bytes.extend_from_slice(b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict/></plist>\n");
    fs::write(path, bytes).unwrap();
}

#[test]
fn mail_is_told_from_its_path() {
    let tmp = tempfile::tempdir().unwrap();
    let file = takeout(tmp.path());
    let what = prepare(Some(&file), None, None).unwrap();
    assert_eq!(
        (what.source(), what.format()),
        (ImportSource::Mbox, "mbox".into())
    );
    assert!(what.is_mail());
    assert_eq!(
        prepare(Some(file.parent().unwrap()), None, None)
            .unwrap()
            .source(),
        ImportSource::Mbox
    );
    let store = tmp.path().join("Library").join("Mail");
    emlx(
        &store.join("V10/ACCOUNT/INBOX.mbox/STORE/Data/Messages/1.emlx"),
        "Message-ID: <a@example.com>\nSubject: Hi\n\nHi.\n",
    );
    let what = prepare(Some(&store), None, None).unwrap();
    assert_eq!(
        (what.source(), what.format()),
        (ImportSource::AppleMail, "Apple Mail".into())
    );
    assert_eq!(
        ImportSource::from_id("apple_mail"),
        Some(ImportSource::AppleMail)
    );
    assert_eq!(ImportSource::from_id("mbox"), Some(ImportSource::Mbox));
    // An mbox needs a path.
    assert!(prepare(None, None, Some(ImportSource::Mbox)).is_err());
    // A vault with one mailbox in it is still a vault; named as mail, it is read as mail.
    let vault = tmp.path().join("vault");
    fs::create_dir_all(vault.join("Archive")).unwrap();
    fs::write(vault.join("Plan.md"), "# Plan\n").unwrap();
    fs::copy(&file, vault.join("Archive").join("old.mbox")).unwrap();
    fs::write(vault.join("Archive").join("1.emlx"), "1\nx").unwrap();
    assert_eq!(
        prepare(Some(&vault), None, None).unwrap().source(),
        ImportSource::Markdown
    );
    assert_eq!(
        prepare(Some(&vault.join("Archive")), None, None)
            .unwrap()
            .source(),
        ImportSource::Markdown
    );
    assert!(
        prepare(Some(&vault), None, Some(ImportSource::Mbox))
            .unwrap()
            .is_mail()
    );
}

#[test]
fn imports_an_mbox_through_the_service_with_filters() {
    let f = Fixture::new();
    let file = takeout(&f.root());
    // As a client sends them.
    let options: MailOptions =
        serde_json::from_value(serde_json::json!({ "folders": ["work"], "until": "2026-03-31" }))
            .unwrap();
    let result = f
        .person
        .import_mail(
            f.space(),
            Some(&file),
            Some(ImportSource::Mbox),
            &options,
            &me(),
        )
        .unwrap();
    assert_eq!(result.source, ImportSource::Mbox);
    assert_eq!(result.created, 1);
    assert_eq!(result.nodes[0].title, "Quarter plan");
    assert_eq!(result.nodes[0].outcome, ImportOutcome::Created);
    assert!(
        result.notes.iter().any(|n| n == "2 messages in 1 thread"),
        "{:?}",
        result.notes
    );
    assert!(
        result
            .notes
            .iter()
            .any(|n| n == "1 message left out by the folder and date filters"),
        "{:?}",
        result.notes
    );

    // Without filters, and again: only the new thread is new.
    let result = f
        .person
        .import_mail(f.space(), Some(&file), None, &MailOptions::default(), &me())
        .unwrap();
    assert_eq!((result.created, result.unchanged), (1, 1));
    let again = f
        .person
        .import_path(f.space(), Some(&file), None, &me())
        .unwrap();
    assert_eq!((again.created, again.updated, again.unchanged), (0, 0, 2));

    let path = f.space();
    let Fixture {
        dir: _dir,
        _service: service,
        person,
    } = f;
    drop(person);
    drop(service);
    let space = Space::open(path).unwrap();
    let kinds_found: Vec<String> = space
        .recent(100)
        .unwrap()
        .into_iter()
        .map(|n| n.kind)
        .collect();
    assert_eq!(
        kinds_found.iter().filter(|k| *k == kinds::THREAD).count(),
        2
    );
    assert_eq!(
        kinds_found.iter().filter(|k| *k == kinds::PERSON).count(),
        2
    );
}

#[test]
fn mail_options_go_only_with_mail() {
    let f = Fixture::new();
    let notes = f.root().join("Plan.md");
    fs::write(&notes, "# Plan\n").unwrap();
    let error = match f.person.import_mail(
        f.space(),
        Some(&notes),
        None,
        &MailOptions {
            attachments: true,
            ..MailOptions::default()
        },
        &me(),
    ) {
        Err(Error::Rpc(e)) => e,
        other => panic!("expected an error, got {other:?}"),
    };
    assert_eq!(error.code, codes::INVALID_PARAMS);
}

#[cfg(unix)]
#[test]
fn says_when_full_disk_access_is_missing() {
    use std::os::unix::fs::PermissionsExt;

    let f = Fixture::new();
    let store = f.root().join("Library").join("Mail");
    emlx(
        &store.join("V10/ACCOUNT/INBOX.mbox/STORE/Data/Messages/1.emlx"),
        "Message-ID: <a@example.com>\nSubject: Hi\n\nHi.\n",
    );
    fs::set_permissions(&store, fs::Permissions::from_mode(0o000)).unwrap();
    let readable = fs::read_dir(&store).is_ok();
    let result = f.person.import_mail(
        f.space(),
        Some(&store),
        Some(ImportSource::AppleMail),
        &MailOptions::default(),
        &me(),
    );
    fs::set_permissions(&store, fs::Permissions::from_mode(0o755)).unwrap();
    if readable {
        return;
    }
    let error = match result {
        Err(Error::Rpc(e)) => e,
        other => panic!("expected an error, got {other:?}"),
    };
    assert_eq!(error.code, codes::NO_ACCESS);
    assert_eq!(error.kind(), Some("no_access"));
    assert!(
        error.message.contains("Full Disk Access"),
        "{}",
        error.message
    );
}
