//! Importing mail from synthetic mailboxes written here: a Google Takeout `.mbox` and an Apple
//! Mail store. Every person and address is invented.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use knode_format::{Node, NodeSummary, Provenance, Space, link_kinds};
use knode_vault::mail::{
    MailError, MailOptions, MailReport, MailSource, detect, import_mail, keys, kinds,
};

fn me() -> Provenance {
    Provenance::person("Ada")
}

fn space(dir: &Path) -> Space {
    Space::create(dir.join("Mail.knode"), "Mail").unwrap()
}

fn import(space: &mut Space, source: MailSource, path: &Path, options: &MailOptions) -> MailReport {
    import_mail(space, source, path, options, &me()).unwrap()
}

fn of_kind(space: &Space, kind: &str) -> Vec<NodeSummary> {
    let mut nodes: Vec<NodeSummary> = space
        .recent(10_000)
        .unwrap()
        .into_iter()
        .filter(|n| n.kind == kind)
        .collect();
    nodes.sort_by(|a, b| a.title.cmp(&b.title));
    nodes
}

fn thread(space: &Space, title: &str) -> Node {
    let found: Vec<_> = of_kind(space, kinds::THREAD)
        .into_iter()
        .filter(|n| n.title == title)
        .collect();
    assert_eq!(found.len(), 1, "one thread titled {title:?}");
    space.get_node(&found[0].id).unwrap().unwrap()
}

/// The message ids of a thread's body, in order.
fn message_ids(node: &Node) -> Vec<String> {
    node.body
        .lines()
        .filter_map(|l| l.strip_prefix("<!-- mail "))
        .map(|l| l.split(' ').next().unwrap().to_string())
        .collect()
}

/// A message, with `\n` line ends.
fn message(headers: &[(&str, &str)], body: &str) -> String {
    let mut out = String::new();
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\n"));
    }
    out.push('\n');
    out.push_str(body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// An mbox of these messages, with the `From ` lines and escaping Google Takeout writes.
fn mbox(messages: &[String]) -> String {
    let mut out = String::new();
    for (i, m) in messages.iter().enumerate() {
        out.push_str(&format!(
            "From 17{i:08}@xxx Mon Mar 02 10:00:00 +0000 2026\n"
        ));
        for line in m.lines() {
            let quotes = line.chars().take_while(|c| *c == '>').count();
            if line[quotes..].starts_with("From ") {
                out.push('>');
            }
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

fn garden_root() -> String {
    message(
        &[
            ("X-GM-THRID", "1700000000000000001"),
            ("X-Gmail-Labels", "Inbox,Opened,Garden"),
            ("Message-ID", "<garden-1@example.com>"),
            ("From", "Ada Example <ada@example.com>"),
            (
                "To",
                "Bruno Sample <bruno@example.org>, \"Chloe Test\" <chloe@example.org>",
            ),
            ("Subject", "Garden plans"),
            ("Date", "Mon, 2 Mar 2026 09:00:00 +0000"),
            ("Content-Type", "text/plain; charset=utf-8"),
        ],
        "Shall we plant tomatoes this year?\n\nFrom what I read, March is right.\n",
    )
}

fn garden_reply_html() -> String {
    message(
        &[
            ("X-Gmail-Labels", "Inbox,Garden"),
            ("Message-ID", "<garden-2@example.org>"),
            ("In-Reply-To", "<garden-1@example.com>"),
            ("References", "<garden-1@example.com>"),
            ("From", "=?UTF-8?Q?Bruno_Sample?= <Bruno@Example.org>"),
            ("To", "ADA@example.com"),
            ("Subject", "Re: Garden plans"),
            ("Date", "Tue, 3 Mar 2026 11:30:00 +0100"),
            ("Content-Type", "text/html; charset=utf-8"),
        ],
        "<html><body><p>Yes, <b>tomatoes</b> and beans.</p><ul><li>Seeds</li><li>Soil</li></ul></body></html>",
    )
}

fn garden_reply_qp() -> String {
    message(
        &[
            ("X-Gmail-Labels", "Sent,Garden"),
            ("Message-ID", "<garden-3@example.com>"),
            (
                "References",
                "<garden-1@example.com> <garden-2@example.org>",
            ),
            ("From", "Ada Example <ada@example.com>"),
            ("To", "Bruno Sample <bruno@example.org>"),
            ("Cc", "=?ISO-8859-1?Q?Chlo=E9_Test?= <chloe@example.org>"),
            ("Subject", "=?UTF-8?B?UmU6IEdhcmRlbiBwbGFucw==?="),
            ("Date", "Thu, 5 Mar 2026 08:00:00 +0000"),
            ("Content-Type", "text/plain; charset=iso-8859-1"),
            ("Content-Transfer-Encoding", "quoted-printable"),
        ],
        "Cr=E8me de la cr=E8me. I will buy the seeds.\n\n> Yes, tomatoes and beans.\n",
    )
}

fn garden_reply_no_headers() -> String {
    message(
        &[
            ("X-Gmail-Labels", "Inbox,Garden"),
            ("Message-ID", "<garden-4@example.org>"),
            ("From", "Chloe Test <chloe@example.org>"),
            ("To", "Ada Example <ada@example.com>"),
            ("Subject", "RE: Garden plans"),
            ("Date", "Fri, 6 Mar 2026 18:00:00 +0000"),
        ],
        "Count me in.\n",
    )
}

fn invoice() -> String {
    message(
        &[
            ("X-Gmail-Labels", "Inbox,Bills"),
            ("Message-ID", "<invoice-1@example.net>"),
            ("From", "Billing <billing@example.net>"),
            ("To", "ada@example.com"),
            ("Subject", "Your invoice for March"),
            ("Date", "Sun, 1 Mar 2026 07:00:00 +0000"),
            ("Content-Type", "multipart/mixed; boundary=\"b1\""),
            ("MIME-Version", "1.0"),
        ],
        "--b1\nContent-Type: text/plain; charset=utf-8\n\nThe invoice is attached.\n\
--b1\nContent-Type: application/pdf; name=\"invoice-march.pdf\"\n\
Content-Disposition: attachment; filename=\"invoice-march.pdf\"\nContent-Transfer-Encoding: base64\n\n\
JVBERi0xLjQKJSBzeW50aGV0aWM=\n--b1\nContent-Type: image/png\n\
Content-Disposition: attachment; filename=\"logo.png\"\nContent-Transfer-Encoding: base64\n\n\
iVBORw0KGgo=\n--b1--\n",
    )
}

fn spam() -> String {
    message(
        &[
            ("X-Gmail-Labels", "Spam"),
            ("Message-ID", "<offer@example.net>"),
            ("From", "Offers <offers@example.net>"),
            ("To", "ada@example.com"),
            ("Subject", "You won"),
            ("Date", "Sat, 7 Mar 2026 07:00:00 +0000"),
        ],
        "Nothing.\n",
    )
}

/// A Takeout mbox, its messages not in the order they were sent.
fn takeout(dir: &Path) -> PathBuf {
    let mail = dir.join("Takeout").join("Mail");
    fs::create_dir_all(&mail).unwrap();
    let file = mail.join("All mail Including Spam and Trash.mbox");
    fs::write(
        &file,
        mbox(&[
            garden_reply_qp(),
            invoice(),
            garden_root(),
            spam(),
            garden_reply_no_headers(),
            garden_reply_html(),
        ]),
    )
    .unwrap();
    file
}

#[test]
fn reads_a_takeout_mbox_into_threads_and_people() {
    let tmp = tempfile::tempdir().unwrap();
    let file = takeout(tmp.path());
    assert_eq!(detect(&file), Some(MailSource::Mbox));
    assert_eq!(detect(file.parent().unwrap()), Some(MailSource::Mbox));
    let mut space = space(tmp.path());
    let report = import(&mut space, MailSource::Mbox, &file, &MailOptions::default());

    assert_eq!(report.created.len(), 2);
    assert_eq!(report.messages, 5);
    assert_eq!(report.junk, 1, "the spam is left out");
    assert_eq!(report.attached, 0, "attachments are off by default");
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);

    let garden = thread(&space, "Garden plans");
    assert_eq!(
        message_ids(&garden),
        [
            "garden-1@example.com",
            "garden-2@example.org",
            "garden-3@example.com",
            "garden-4@example.org"
        ],
        "in the order they were sent, the reply without headers joined by its subject"
    );
    let body = &garden.body;
    assert!(body.starts_with("# Garden plans\n"), "{body}");
    assert!(body.contains("## Ada Example - 2026-03-02 09:00"), "{body}");
    assert!(
        body.contains("- **To:** Bruno Sample <bruno@example.org>, Chloe Test <chloe@example.org>"),
        "{body}"
    );
    // HTML only: turned into Markdown.
    assert!(body.contains("Yes, **tomatoes** and beans."), "{body}");
    assert!(body.contains("- Seeds"), "{body}");
    // Quoted-printable in ISO-8859-1, an encoded-word subject and recipient.
    assert!(body.contains("Crème de la crème."), "{body}");
    assert!(
        body.contains("- **Cc:** Chloé Test <chloe@example.org>"),
        "{body}"
    );
    // An escaped `From ` line comes back as it was.
    assert!(
        body.contains("\nFrom what I read, March is right."),
        "{body}"
    );

    let props = space.properties(&garden.id).unwrap();
    assert_eq!(props[keys::THREAD], "garden-1@example.com");
    assert_eq!(props[keys::SOURCE], "mbox");
    assert_eq!(props[keys::MESSAGES], "4");
    assert_eq!(props[keys::FOLDERS], "Garden, Inbox, Sent");
    assert_eq!(props[keys::STARTED_AT], "2026-03-02T09:00:00Z");
    assert_eq!(props[keys::ENDED_AT], "2026-03-06T18:00:00Z");
    assert_eq!(garden.created_at.0, 1_772_442_000_000);

    let invoice = thread(&space, "Your invoice for March");
    assert!(
        invoice.body.contains("_[attached: invoice-march.pdf]_"),
        "{}",
        invoice.body
    );
    assert!(space.attachments(&invoice.id).unwrap().is_empty());

    // One person per address, whatever its case.
    let people = of_kind(&space, kinds::PERSON);
    let titles: Vec<&str> = people.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(
        titles,
        ["Ada Example", "Billing", "Bruno Sample", "Chloe Test"]
    );
    assert_eq!(report.people, 4);
    let bruno = &people[2];
    assert_eq!(
        space
            .property(&bruno.id, keys::PERSON_EMAIL)
            .unwrap()
            .as_deref(),
        Some("bruno@example.org")
    );
    let linked: Vec<_> = space
        .links_from(&garden.id)
        .unwrap()
        .into_iter()
        .filter(|l| l.kind == link_kinds::RELATED)
        .map(|l| l.to)
        .collect();
    assert_eq!(linked.len(), 3, "the thread links to its three people");
    assert!(linked.contains(&bruno.id));
}

#[test]
fn importing_again_creates_no_duplicates() {
    let tmp = tempfile::tempdir().unwrap();
    let file = takeout(tmp.path());
    let mut space = space(tmp.path());
    import(&mut space, MailSource::Mbox, &file, &MailOptions::default());
    let nodes = space.node_count().unwrap();

    let again = import(&mut space, MailSource::Mbox, &file, &MailOptions::default());
    assert!(
        again.created.is_empty() && again.updated.is_empty(),
        "{again:?}"
    );
    assert_eq!(again.unchanged, 2);
    assert_eq!(again.people, 0);
    assert_eq!(space.node_count().unwrap(), nodes);
}

#[test]
fn filters_by_label_and_date_and_adds_to_the_same_thread_later() {
    let tmp = tempfile::tempdir().unwrap();
    let file = takeout(tmp.path());
    let mut space = space(tmp.path());

    let first = MailOptions {
        folders: vec!["garden".into()],
        until: NaiveDate::from_ymd_opt(2026, 3, 3),
        ..MailOptions::default()
    };
    let report = import(&mut space, MailSource::Mbox, &file, &first);
    assert_eq!(report.created.len(), 1);
    assert_eq!(report.messages, 2);
    assert_eq!(report.filtered, 3, "the invoice and two later replies");
    assert!(of_kind(&space, kinds::THREAD).len() == 1);
    assert_eq!(
        message_ids(&thread(&space, "Garden plans")),
        ["garden-1@example.com", "garden-2@example.org"]
    );

    // The rest of the month: the later replies join the thread already there.
    let rest = MailOptions {
        folders: vec!["Garden".into()],
        since: NaiveDate::from_ymd_opt(2026, 3, 4),
        ..MailOptions::default()
    };
    let report = import(&mut space, MailSource::Mbox, &file, &rest);
    assert!(report.created.is_empty(), "{report:?}");
    assert_eq!(report.updated.len(), 1);
    assert_eq!(report.updated[0].messages, 4);
    let garden = thread(&space, "Garden plans");
    assert_eq!(
        message_ids(&garden),
        [
            "garden-1@example.com",
            "garden-2@example.org",
            "garden-3@example.com",
            "garden-4@example.org"
        ]
    );
    assert_eq!(
        space
            .property(&garden.id, keys::MESSAGES)
            .unwrap()
            .as_deref(),
        Some("4")
    );

    // An empty range is refused.
    let empty = MailOptions {
        since: NaiveDate::from_ymd_opt(2026, 4, 1),
        until: NaiveDate::from_ymd_opt(2026, 3, 1),
        ..MailOptions::default()
    };
    assert!(matches!(
        import_mail(&mut space, MailSource::Mbox, &file, &empty, &me()),
        Err(MailError::Invalid(_))
    ));

    // Asking for spam brings it in.
    let spam = MailOptions {
        folders: vec!["Spam".into()],
        ..MailOptions::default()
    };
    let report = import(&mut space, MailSource::Mbox, &file, &spam);
    assert_eq!(report.created.len(), 1);
    assert_eq!(report.junk, 0);
}

#[test]
fn attaches_files_when_asked() {
    let tmp = tempfile::tempdir().unwrap();
    let file = takeout(tmp.path());
    let mut space = space(tmp.path());
    let options = MailOptions {
        attachments: true,
        ..MailOptions::default()
    };
    let report = import(&mut space, MailSource::Mbox, &file, &options);
    assert_eq!(report.attached, 2);
    let invoice = thread(&space, "Your invoice for March");
    assert!(
        invoice.body.contains("[[invoice-march.pdf]]"),
        "{}",
        invoice.body
    );
    assert!(invoice.body.contains("![[logo.png]]"), "{}", invoice.body);
    let attached = space.attachments(&invoice.id).unwrap();
    let names: Vec<&str> = attached.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["invoice-march.pdf", "logo.png"]);
    let pdf = space.read_blob(&attached[0].hash).unwrap().unwrap();
    assert!(pdf.starts_with(b"%PDF-1.4"));

    let again = import(&mut space, MailSource::Mbox, &file, &options);
    assert_eq!(again.attached, 0);
    assert_eq!(again.unchanged, 2);
    assert_eq!(space.attachments(&invoice.id).unwrap().len(), 2);
}

/// Writes an `.emlx` file: the length of the message, the message, then a property list.
fn emlx(path: &Path, message: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = format!("{}\n", message.len()).into_bytes();
    bytes.extend_from_slice(message.as_bytes());
    bytes.extend_from_slice(
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>flags</key>\
<integer>8590195713</integer></dict></plist>\n",
    );
    fs::write(path, bytes).unwrap();
}

/// A synthetic Apple Mail store, as macOS lays it out under `~/Library/Mail`.
fn apple_store(dir: &Path) -> PathBuf {
    let mail = dir.join("Library").join("Mail");
    // An older store left behind by an upgrade: never read.
    emlx(
        &mail.join("V9/ACCOUNT/INBOX.mbox/Messages/1.emlx"),
        &message(
            &[("Message-ID", "<old@example.com>"), ("Subject", "Old")],
            "Old.\n",
        ),
    );
    let account = mail.join("V10").join("0F1E2D3C-ACCOUNT");
    let messages = |mailbox: &str| account.join(mailbox).join("1A2B3C4D-STORE/Data/1/Messages");
    let first = message(
        &[
            ("Message-ID", "<trip-1@example.com>"),
            ("From", "Dana Sample <dana@example.com>"),
            ("To", "Ada Example <ada@example.com>"),
            ("Subject", "Trip to the coast"),
            ("Date", "Wed, 4 Feb 2026 12:00:00 -0500"),
        ],
        "Train or car?\n",
    );
    emlx(&messages("INBOX.mbox").join("101.emlx"), &first);
    // The same message in Gmail's All Mail: imported once.
    emlx(
        &messages("[Gmail].mbox/All Mail.mbox").join("201.emlx"),
        &first,
    );
    emlx(
        &messages("Archive.mbox").join("102.partial.emlx"),
        &message(
            &[
                ("Message-ID", "<trip-2@example.com>"),
                ("In-Reply-To", "<trip-1@example.com>"),
                ("From", "Ada Example <ada@example.com>"),
                ("To", "Dana Sample <dana@example.com>"),
                ("Subject", "Re: Trip to the coast"),
                ("Date", "Thu, 5 Feb 2026 08:00:00 +0000"),
                ("Content-Type", "multipart/mixed; boundary=b"),
            ],
            "--b\nContent-Type: text/plain\n\nTrain. Tickets attached.\n--b\n\
Content-Type: application/pdf\nContent-Disposition: attachment; filename=tickets.pdf\n\
X-Apple-Content-Length: 1200\n\n--b--\n",
        ),
    );
    emlx(
        &messages("Junk.mbox").join("103.emlx"),
        &message(
            &[
                ("Message-ID", "<junk@example.net>"),
                ("From", "offers@example.net"),
                ("Subject", "Cheap"),
                ("Date", "Thu, 5 Feb 2026 08:00:00 +0000"),
            ],
            "No.\n",
        ),
    );
    fs::create_dir_all(mail.join("V10/MailData")).unwrap();
    fs::write(mail.join("V10/MailData/Envelope Index"), b"not mail").unwrap();
    mail
}

#[test]
fn reads_the_apple_mail_store() {
    let tmp = tempfile::tempdir().unwrap();
    let mail = apple_store(tmp.path());
    assert_eq!(detect(&mail), Some(MailSource::AppleMail));
    let mut space = space(tmp.path());
    let options = MailOptions {
        attachments: true,
        ..MailOptions::default()
    };
    let report = import(&mut space, MailSource::AppleMail, &mail, &options);
    assert_eq!(report.source, MailSource::AppleMail);
    assert_eq!(report.created.len(), 1, "{report:?}");
    assert_eq!(report.messages, 2);
    assert_eq!(report.duplicates, 1);
    assert_eq!(report.junk, 1);
    assert_eq!(
        report.attached, 0,
        "the partial message's files are elsewhere"
    );
    assert_eq!(report.limits.len(), 1);

    let trip = thread(&space, "Trip to the coast");
    assert_eq!(
        message_ids(&trip),
        ["trip-1@example.com", "trip-2@example.com"]
    );
    assert!(
        trip.body.contains("## Dana Sample - 2026-02-04 12:00"),
        "{}",
        trip.body
    );
    assert!(
        trip.body.contains("_[attached: tickets.pdf]_"),
        "{}",
        trip.body
    );
    assert_eq!(
        space.property(&trip.id, keys::FOLDERS).unwrap().as_deref(),
        Some("Archive, INBOX, [Gmail]/All Mail")
    );
    assert_eq!(of_kind(&space, kinds::PERSON).len(), 2);

    // One mailbox.
    let mut other = Space::create(tmp.path().join("Other.knode"), "Other").unwrap();
    let archive = MailOptions {
        folders: vec!["archive".into()],
        ..MailOptions::default()
    };
    let report = import(&mut other, MailSource::AppleMail, &mail, &archive);
    assert_eq!(report.messages, 1);
    assert_eq!(report.filtered, 2);

    let again = import(&mut space, MailSource::AppleMail, &mail, &options);
    assert!(
        again.created.is_empty() && again.updated.is_empty(),
        "{again:?}"
    );
}

#[cfg(unix)]
#[test]
fn says_how_to_allow_full_disk_access() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let mail = apple_store(tmp.path());
    let store = mail.join("V10");
    fs::set_permissions(&store, fs::Permissions::from_mode(0o000)).unwrap();
    let readable = fs::read_dir(&store).is_ok();
    let mut space = space(tmp.path());
    let result = import_mail(
        &mut space,
        MailSource::AppleMail,
        &mail,
        &MailOptions::default(),
        &me(),
    );
    fs::set_permissions(&store, fs::Permissions::from_mode(0o755)).unwrap();
    if readable {
        // Running as a user the permissions do not stop, such as root.
        return;
    }
    let error = result.unwrap_err();
    assert!(matches!(error, MailError::NoAccess { .. }), "{error:?}");
    assert!(error.to_string().contains("Full Disk Access"), "{error}");
    assert_eq!(space.node_count().unwrap(), 0);
}

#[test]
fn refuses_what_is_not_mail() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = space(tmp.path());
    let missing = tmp.path().join("missing.mbox");
    assert!(matches!(
        import_mail(
            &mut space,
            MailSource::Mbox,
            &missing,
            &MailOptions::default(),
            &me()
        ),
        Err(MailError::NotFound(_))
    ));
    let notes = tmp.path().join("notes");
    fs::create_dir_all(&notes).unwrap();
    fs::write(notes.join("a.md"), "# A\n").unwrap();
    assert_eq!(detect(&notes), None);
    assert!(matches!(
        import_mail(
            &mut space,
            MailSource::AppleMail,
            &notes,
            &MailOptions::default(),
            &me()
        ),
        Err(MailError::NotRecognized { .. })
    ));
    assert!(matches!(
        import_mail(
            &mut space,
            MailSource::Mbox,
            &notes,
            &MailOptions::default(),
            &me()
        ),
        Err(MailError::NotRecognized { .. })
    ));
}
