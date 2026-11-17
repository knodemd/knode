//! `knode import` of mail: a synthetic `.mbox` and a synthetic Apple Mail store in the sandbox's
//! home. Every person and address is invented.

mod common;

use std::fs;
use std::path::Path;

use common::Sandbox;

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn message(id: &str, labels: &str, subject: &str, date: &str, extra: &str) -> String {
    format!(
        "From 1@xxx Mon Mar 02 10:00:00 +0000 2026\nX-Gmail-Labels: {labels}\nMessage-ID: <{id}>\n\
From: Ada Example <ada@example.com>\nTo: Bruno Sample <bruno@example.org>\nSubject: {subject}\n\
Date: {date}\n{extra}\nHello from {id}.\n\n"
    )
}

fn mbox(dir: &Path) -> std::path::PathBuf {
    fs::create_dir_all(dir).unwrap();
    let file = dir.join("All mail Including Spam and Trash.mbox");
    let mut text = message(
        "plan-1@example.com",
        "Inbox,Work",
        "Quarter plan",
        "Mon, 2 Mar 2026 09:00:00 +0000",
        "",
    );
    text.push_str(&message(
        "plan-2@example.org",
        "Work",
        "Re: Quarter plan",
        "Tue, 3 Mar 2026 09:00:00 +0000",
        "References: <plan-1@example.com>\n",
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

#[test]
fn imports_an_mbox_with_filters() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let file = mbox(&sb.path("Takeout/Mail"));

    let out = sb.ok(&[
        "import",
        s(&file),
        "--label",
        "Work",
        "--until",
        "2026-03-31",
    ]);
    assert!(
        out.starts_with(&format!(
            "Imported {} (mbox): 1 new, 0 updated, 0 unchanged.",
            file.display()
        )),
        "{out}"
    );
    assert!(out.contains("\n  new: Quarter plan\n"), "{out}");
    assert!(out.contains("2 messages in 1 thread"), "{out}");
    assert!(
        out.contains("1 message left out by the folder and date filters"),
        "{out}"
    );

    let out = sb.ok(&["import", s(&file), "--from", "mbox"]);
    assert!(out.contains(": 1 new, 0 updated, 1 unchanged."), "{out}");
    let out = sb.ok(&["import", s(&file)]);
    assert!(out.contains(": 0 new, 0 updated, 2 unchanged."), "{out}");

    let space = knode_format::Space::open(sb.space("work")).unwrap();
    let plan = space.find_by_title("Quarter plan").unwrap();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].kind, "email");
    let node = space.get_node(&plan[0].id).unwrap().unwrap();
    assert_eq!(node.created_by.actor, "tester");
    assert_eq!(
        space.find_by_title("Bruno Sample").unwrap()[0].kind,
        "person"
    );
}

#[test]
fn refuses_mail_options_for_other_sources_and_bad_days() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let note = sb.path("Plan.md");
    fs::write(&note, "# Plan\n").unwrap();
    let err = sb.fails(&["import", s(&note), "--attachments"]);
    assert!(err.contains("are for mail"), "{err}");
    let file = mbox(&sb.path("Takeout/Mail"));
    let err = sb.fails(&["import", s(&file), "--since", "March"]);
    assert!(err.contains("YYYY-MM-DD"), "{err}");
}

#[test]
fn reads_apple_mail_from_the_home_folder() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let messages = sb
        .home()
        .join("Library/Mail/V10/ACCOUNT/INBOX.mbox/STORE/Data/Messages");
    fs::create_dir_all(&messages).unwrap();
    let mail = "Message-ID: <trip@example.com>\nFrom: Dana Sample <dana@example.com>\n\
To: ada@example.com\nSubject: Trip\nDate: Wed, 4 Feb 2026 12:00:00 +0000\n\nTrain?\n";
    let mut bytes = format!("{}\n", mail.len()).into_bytes();
    bytes.extend_from_slice(mail.as_bytes());
    bytes.extend_from_slice(b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict/></plist>\n");
    fs::write(messages.join("1.emlx"), bytes).unwrap();

    if cfg!(target_os = "macos") {
        let out = sb.ok(&["import", "--from", "apple-mail", "--mailbox", "INBOX"]);
        assert!(out.contains("(Apple Mail): 1 new"), "{out}");
        assert!(out.contains("new: Trip"), "{out}");
    } else {
        let err = sb.fails(&["import", "--from", "apple-mail"]);
        assert!(err.contains("on a Mac only"), "{err}");
    }
}
