//! Mail read from local files into a space with [`import_mail`]: Gmail through a Google
//! Takeout `.mbox` file, and the Apple Mail store on the Mac. Knode never signs in to a mail
//! account: it reads the files the person points it at, and writes only into their space.
//!
//! | Source | What is read | Status |
//! | --- | --- | --- |
//! | Gmail ([`MailSource::Mbox`]) | The `.mbox` file of a Google Takeout export of Mail, a folder of `.mbox` files, or any other `mbox` file (such as a mailbox Apple Mail exported) | Labels from `X-Gmail-Labels`; a plain `mbox` without labels gets the file's name as its folder |
//! | Apple Mail ([`MailSource::AppleMail`]) | The local store, `~/Library/Mail/V<n>` (macOS, needs Full Disk Access), any folder of `.emlx` files, or a mailbox Apple Mail exported (`<name>.mbox`, read as an `mbox`) | The mailbox is the path of `.mbox` folders a message is in; attachments Apple Mail keeps outside the message (`.partial.emlx`) are not read |
//!
//! # Threads
//!
//! Every thread becomes one node of kind [`kinds::THREAD`], titled with its subject (without
//! `Re:` and `Fwd:`). Messages are grouped by `Message-ID`, `In-Reply-To` and `References`; a
//! reply that has neither of the last two joins the thread with the same subject only when it
//! shares one of the thread's people and was sent at most 30 days after its last message. When
//! one message joins two threads written before, it goes to the older node, each message stays
//! in the one node it is in, and the two nodes are linked as `related`. The body has
//! one section per message, in the order they were sent: a `##` heading with the sender and the
//! date, the sender and the recipients, then the text. The text part is preferred; HTML is
//! turned into Markdown when a message has no text part. Every section starts with a
//! `<!-- mail <message-id> <date> <lines> <digest> -->` line, which is how the next import finds
//! it and tells whether it was changed in Knode.
//!
//! # People
//!
//! Everyone who sent or received a message of a thread becomes one node of kind
//! [`kinds::PERSON`], found again by their address ([`keys::PERSON_EMAIL`], in lower case), and
//! the thread links to them as `related`. A person is named by the first name a message gives
//! them. A later import leaves a person node as it is, so names and notes added in Knode are
//! kept; only a title that is the bare address becomes the name once a message gives one.
//!
//! # Filters and attachments
//!
//! [`MailOptions`] keeps the import to some folders (Gmail labels or Apple Mail mailboxes) and
//! dates, and turns on attachments, which are off by default. Spam and trash (Gmail's `Spam` and
//! `Trash` labels, mailboxes named `Junk`, `Spam`, `Trash`, `Bin` or `Deleted Messages`) are left
//! out unless a folder filter names them.
//!
//! # Importing again
//!
//! A thread is found again by [`keys::THREAD`], so importing the same mailbox again, a newer
//! export, or another date range adds the new messages to the thread's node instead of making a
//! new one. What someone wrote in Knode is never lost: the section of a message read again is
//! written again from the mail only while it is as Knode wrote it; a section changed in Knode is
//! left as it is, and text added between sections or after the last one is kept where it is.
//! Files attached by an earlier import stay linked when an import without attachments runs.

mod apple;
mod mbox;
mod message;
mod raw;
mod thread;
mod write;

use std::fmt;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use knode_format::{NodeId, Provenance, Space};
use serde::{Deserialize, Serialize};

pub use crate::Skipped;
pub use raw::{RawMail, import_raw_mail};

/// The kinds of node the mail importer writes.
pub mod kinds {
    /// A mail thread: its messages, in order.
    pub const THREAD: &str = "email";
    /// Someone who sent or received mail.
    pub const PERSON: &str = "person";
}

/// The node properties the mail importer writes.
pub mod keys {
    /// What finds a thread again: the `Message-ID` its first message names as the start of the
    /// thread, without `<` and `>`.
    pub const THREAD: &str = "mail.thread";
    /// The subject of the thread, without `Re:` and `Fwd:`, in lower case: a reply without
    /// `In-Reply-To` and `References` finds its thread by it.
    pub const SUBJECT: &str = "mail.subject";
    /// Where the thread was read from: `mbox` or `apple-mail`.
    pub const SOURCE: &str = "mail.source";
    /// How many messages the thread's node has.
    pub const MESSAGES: &str = "mail.messages";
    /// The folders the thread's messages are in (Gmail labels or Apple Mail mailboxes),
    /// separated by `, `.
    pub const FOLDERS: &str = "mail.folders";
    /// When the first message was sent, RFC 3339 in UTC.
    pub const STARTED_AT: &str = "mail.started_at";
    /// When the last message was sent, RFC 3339 in UTC.
    pub const ENDED_AT: &str = "mail.ended_at";
    /// The address of a person, in lower case, which finds them again.
    pub const PERSON_EMAIL: &str = "person.email";
}

/// Where mail is read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum MailSource {
    /// An `mbox` file, such as the one Google Takeout exports Gmail as, or a folder of them.
    Mbox,
    /// The Apple Mail store on the Mac, or a folder of `.emlx` files.
    AppleMail,
    /// A mail account read over IMAP, live ([`import_raw_mail`]).
    Imap,
    /// Gmail read through the Gmail API, live ([`import_raw_mail`]).
    Gmail,
}

impl MailSource {
    /// The id kept in [`keys::SOURCE`]: `mbox`, `apple-mail`, `imap` or `gmail`.
    pub fn id(self) -> &'static str {
        match self {
            MailSource::Mbox => "mbox",
            MailSource::AppleMail => "apple-mail",
            MailSource::Imap => "imap",
            MailSource::Gmail => "gmail",
        }
    }
}

impl fmt::Display for MailSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            MailSource::Mbox => "mbox",
            MailSource::AppleMail => "Apple Mail",
            MailSource::Imap => "IMAP",
            MailSource::Gmail => "Gmail",
        })
    }
}

/// What to import, beyond the path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MailOptions {
    /// Only messages in one of these folders: Gmail labels (`Inbox`, `Work/Clients`) or Apple
    /// Mail mailboxes (`INBOX`, `Archive`). Names are compared without case, and a name matches
    /// a whole folder path or its last part. Empty: every folder.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<String>,
    /// Only messages sent on or after this day, in UTC.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<NaiveDate>,
    /// Only messages sent on or before this day, in UTC.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<NaiveDate>,
    /// Attach the files of the messages to their thread's node. Off by default.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub attachments: bool,
}

impl MailOptions {
    /// Whether every option is left as it is by default.
    pub fn is_default(&self) -> bool {
        *self == MailOptions::default()
    }
}

/// A day as [`MailOptions::since`] and [`MailOptions::until`] take it: `YYYY-MM-DD`.
pub fn parse_day(text: &str) -> Result<NaiveDate, MailError> {
    NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d")
        .map_err(|_| MailError::Invalid(format!("{text:?} is not a day: write it as YYYY-MM-DD")))
}

/// Why [`import_mail`] could not read the mail. Messages that cannot be read one by one do not
/// fail the import: they are listed in [`MailReport::skipped`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MailError {
    /// macOS keeps Knode from reading Apple Mail's store: the app running Knode needs Full Disk
    /// Access.
    #[error(
        "macOS does not let Knode read {} (Apple Mail keeps its mail there): only apps with Full \
         Disk Access can. Open System Settings > Privacy & Security > Full Disk Access, turn it \
         on for the app that runs Knode (Knode, or your terminal), then quit and reopen that \
         app. Or export the mailboxes from Apple Mail (Mailbox > Export Mailbox) and import the \
         exported .mbox.",
        path.display()
    )]
    NoAccess {
        /// The folder or file that could not be read.
        path: PathBuf,
    },
    /// Nothing at the path.
    #[error("{} does not exist", .0.display())]
    NotFound(PathBuf),
    /// The path is not mail Knode reads.
    #[error("{} is not {what}", path.display())]
    NotRecognized {
        /// The path.
        path: PathBuf,
        /// What it should have been.
        what: String,
    },
    /// The options do not go together.
    #[error("{0}")]
    Invalid(String),
    /// A file could not be read.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        source: std::io::Error,
    },
    /// The space returned an error.
    #[error(transparent)]
    Space(#[from] knode_format::Error),
}

/// Turns a failure to read `path` into [`MailError::NoAccess`] when macOS refused it, else into
/// [`MailError::Io`].
pub(crate) fn read_error(path: &Path) -> impl FnOnce(std::io::Error) -> MailError + '_ {
    move |source| {
        if source.kind() == std::io::ErrorKind::PermissionDenied {
            MailError::NoAccess {
                path: path.to_path_buf(),
            }
        } else {
            MailError::Io {
                path: path.to_path_buf(),
                source,
            }
        }
    }
}

/// One thread node [`import_mail`] created or changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedThread {
    /// The node.
    pub node_id: NodeId,
    /// What finds it again ([`keys::THREAD`]).
    pub thread: String,
    /// Its title.
    pub title: String,
    /// How many messages it has now.
    pub messages: usize,
}

/// What [`import_mail`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailReport {
    /// Where the mail was read from.
    pub source: MailSource,
    /// The thread nodes created.
    pub created: Vec<ImportedThread>,
    /// The thread nodes that got new or changed messages.
    pub updated: Vec<ImportedThread>,
    /// How many threads were already up to date.
    pub unchanged: usize,
    /// How many messages were read into threads.
    pub messages: usize,
    /// How many of them were not in the space before: the rest were there already, from this
    /// import or another one of the same messages (found by their `Message-ID`).
    #[serde(default)]
    pub added: usize,
    /// How many person nodes were created.
    pub people: usize,
    /// How many files were attached.
    pub attached: usize,
    /// How many messages the folder and date filters left out.
    pub filtered: usize,
    /// How many messages were spam or trash, left out.
    pub junk: usize,
    /// How many messages were read twice (the same `Message-ID` in two folders) and imported
    /// once.
    pub duplicates: usize,
    /// The messages and files that could not be imported, and why. `path` is the file, or the
    /// message's subject.
    pub skipped: Vec<Skipped>,
    /// What the import read only in part, one line each.
    pub limits: Vec<String>,
}

impl MailReport {
    fn new(source: MailSource) -> Self {
        Self {
            source,
            created: Vec::new(),
            updated: Vec::new(),
            unchanged: 0,
            messages: 0,
            added: 0,
            people: 0,
            attached: 0,
            filtered: 0,
            junk: 0,
            duplicates: 0,
            skipped: Vec::new(),
            limits: Vec::new(),
        }
    }

    /// How many threads the import read: created, updated or unchanged.
    pub fn read_threads(&self) -> usize {
        self.created.len() + self.updated.len() + self.unchanged
    }

    pub(crate) fn skip(&mut self, what: impl Into<String>, reason: impl Into<String>) {
        self.skipped.push(Skipped {
            path: what.into(),
            reason: reason.into(),
        });
    }

    /// Adds a limit, once.
    pub(crate) fn limit(&mut self, text: impl Into<String>) {
        let text = text.into();
        if !self.limits.contains(&text) {
            self.limits.push(text);
        }
    }
}

/// Whether mail in `folder` (a Gmail label or an Apple Mail mailbox, such as `[Gmail]/Spam`
/// or `Junk`) is spam or trash, left out unless a folder filter names it: its last part is
/// `Spam`, `Junk`, `Trash`, `Bin`, `Deleted Messages` or `Deleted Items`, in any case.
pub fn is_junk_folder(folder: &str) -> bool {
    message::is_junk_folder(folder)
}

/// Where Apple Mail keeps its mail on this Mac, `~/Library/Mail`; `None` off macOS. Nothing is
/// read: [`import_mail`] picks the newest `V<n>` folder in it.
pub fn apple_mail_location() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    Some(home.join("Library").join("Mail"))
}

/// Where the mail at `path` comes from, when the path itself is mail:
///
/// - [`MailSource::Mbox`]: an `.mbox` file, a file named `mbox` that starts like one, a mailbox
///   Apple Mail exported (an `.mbox` folder with an `mbox` file in it), or a folder that holds
///   only `.mbox` files, such as the `Mail` folder of a Google Takeout export;
/// - [`MailSource::AppleMail`]: an `.emlx` file, Apple Mail's `~/Library/Mail` or a `V<n>`
///   folder in it, or one of its mailbox folders (`INBOX.mbox`).
///
/// A folder that holds other things too, such as notes with one `.mbox` among them, is not mail:
/// name the source to import it as mail.
pub fn detect(path: impl AsRef<Path>) -> Option<MailSource> {
    let path = path.as_ref();
    if path.is_file() {
        let name = file_name(path);
        if name.ends_with(".emlx") {
            return Some(MailSource::AppleMail);
        }
        if name.ends_with(".mbox") || (name == "mbox" && mbox::starts_like_mbox(path)) {
            return Some(MailSource::Mbox);
        }
        return None;
    }
    if apple::is_store(path) {
        return Some(MailSource::AppleMail);
    }
    if !path.is_dir() {
        return None;
    }
    if mbox::is_exported_mailbox(path) || mbox::only_mbox_files(path) {
        return Some(MailSource::Mbox);
    }
    if apple::is_mailbox(path) {
        return Some(MailSource::AppleMail);
    }
    None
}

/// The file name of `path` in lower case.
pub(crate) fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Imports the mail at `path`, read as `source`, into `space`, written by `by` (with `path` as
/// the [`source`](Provenance::source) when `by` has none). See the [module](self) for what is
/// written.
///
/// `path` is what [`detect`] lists for the source; for Apple Mail, it can be
/// [`apple_mail_location`]. Mail files are only ever read. Without Full Disk Access, reading
/// Apple Mail's store fails with [`MailError::NoAccess`], which says how to allow it.
///
/// ```no_run
/// use knode_format::{Provenance, Space};
/// use knode_vault::mail::{MailOptions, MailSource, import_mail};
///
/// let mut space = Space::open("/Users/example/Knode/Personal.knode")?;
/// let options = MailOptions { folders: vec!["Work".into()], ..MailOptions::default() };
/// let report = import_mail(&mut space, MailSource::Mbox,
///     "/Users/example/Downloads/Takeout/Mail/All mail Including Spam and Trash.mbox",
///     &options, &Provenance::person("example"))?;
/// println!("{} new threads", report.created.len());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn import_mail(
    space: &mut Space,
    source: MailSource,
    path: impl AsRef<Path>,
    options: &MailOptions,
    by: &Provenance,
) -> Result<MailReport, MailError> {
    let path = path.as_ref();
    if let (Some(since), Some(until)) = (options.since, options.until)
        && since > until
    {
        return Err(MailError::Invalid(format!(
            "the date range is empty: {since} is after {until}"
        )));
    }
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(MailError::NotFound(path.to_path_buf()));
        }
        Err(e) => return Err(read_error(path)(e)),
        Ok(_) => {}
    }
    let by = if by.source.is_some() {
        by.clone()
    } else {
        by.clone().with_source(path.to_string_lossy())
    };
    let mut report = MailReport::new(source);
    let mut collected = message::Collector::new(options);
    match source {
        MailSource::Mbox => mbox::read(path, &mut collected, &mut report)?,
        MailSource::AppleMail => apple::read(path, options, &mut collected, &mut report)?,
        MailSource::Imap | MailSource::Gmail => {
            return Err(MailError::Invalid(format!(
                "{source} is read live by its connector, not from a file: import an .mbox instead"
            )));
        }
    }
    report.filtered = collected.filtered;
    report.junk = collected.junk;
    report.duplicates = collected.duplicates;
    let threads = thread::group(collected.into_mails());
    write::write(space, source, threads, options, &by, &mut report)?;
    Ok(report)
}

/// Imports some `.emlx` files of Apple Mail's store into `space`, written by `by`: the
/// messages a connector found new. `root` is the store's `V<n>` folder; each file's mailbox
/// is the path of `.mbox` folders between it and the file, as [`import_mail`] names it, so
/// both find the same threads. Messages already in the space, from any import of the same
/// mail (an Apple Mail store or export, a Gmail Takeout `.mbox`), are found by their
/// `Message-ID` and added only once; [`MailReport::added`] counts the new ones.
pub fn import_apple_mail_files(
    space: &mut Space,
    root: impl AsRef<Path>,
    files: &[PathBuf],
    options: &MailOptions,
    by: &Provenance,
) -> Result<MailReport, MailError> {
    let root = root.as_ref();
    let source = MailSource::AppleMail;
    let mut report = MailReport::new(source);
    let mut collected = message::Collector::new(options);
    apple::read_some(root, files, options, &mut collected, &mut report)?;
    report.filtered = collected.filtered;
    report.junk = collected.junk;
    report.duplicates = collected.duplicates;
    let threads = thread::group(collected.into_mails());
    write::write(space, source, threads, options, by, &mut report)?;
    Ok(report)
}
