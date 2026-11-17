//! Importing chat exports into a space: [`import_chat_export`].
//!
//! | Export | Format id | What it reads |
//! | --- | --- | --- |
//! | WhatsApp "Export chat" | `whatsapp` | The `.zip`, its folder, or the `.txt`: `_chat.txt` (iOS) or `WhatsApp Chat with <name>.txt` (Android), in every locale, with the media beside it |
//! | Slack workspace export | `slack` | The `.zip` or its folder: `users.json`, `channels.json` (and `groups.json`, `mpims.json`, `dms.json`) and a folder of day files per conversation; threads become their own nodes, members `person` nodes |
//! | Telegram Desktop, JSON | `telegram` | The folder with `result.json` (one chat, or the whole account, read one chat at a time), or a `.zip` of it, with the media beside it |
//!
//! Every chat becomes one node of kind [`CHAT_KIND`]: the messages grouped by day under
//! `## YYYY-MM-DD` headings, each with its sender and time, and the files of the export
//! attached to the node. Its properties ([`keys`]) say which app and chat it is, who wrote in
//! it and when. Importing the same chat again, or a newer export of it, adds only the messages
//! after the last one imported: none is written twice.
//!
//! What is shaped like a credential in the messages (API keys and tokens in known formats, a
//! long value of letters and digits after `password:` or `token=`, the password in a URL) is
//! replaced with `[redacted]`; words people wrote after "Secret:" are kept. The access
//! token in the private URL of a Slack file is left out, so a credential pasted into a chat is
//! not stored in the space.
//!
//! Everything is read from the files the person gives and written only into the space they
//! pick; nothing is sent anywhere.

mod files;
pub mod imessage;
mod model;
mod render;
mod slack;
mod telegram;
mod whatsapp;
pub mod whatsapp_mac;
mod write;

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use knode_format::{Provenance, Space};

use crate::exports::{Imported, Skipped};
use files::{Files, file_name};
use write::ChatWriter;

/// The kind of the node of a chat.
pub const CHAT_KIND: &str = "chat";

/// The kind of the node of a Slack member.
pub const PERSON_KIND: &str = slack::PERSON_KIND;

pub use slack::person_keys;

/// The properties of a chat's node.
pub mod keys {
    /// The app: `whatsapp`, `telegram`, `slack` or `imessage`.
    pub const APP: &str = "chat.app";
    /// The chat's id in the app, or, for apps that have none, one made from its name.
    pub const ID: &str = "chat.id";
    /// The chat's own id in the app, when Knode read it from the app itself: a WhatsApp JID
    /// (`<number>@s.whatsapp.net`, `<id>@g.us`). Exports carry none; a chat is found by its JID
    /// first and by [`ID`] when no node has it, and the JID is then stored on that node.
    pub const JID: &str = "chat.jid";
    /// Where the last message written arrived in the app, when the app numbers its messages in
    /// that order (WhatsApp for Mac: the message's `Z_PK`). Messages past it are new, whenever
    /// they were sent.
    pub const LAST_SEQ: &str = "chat.last_seq";
    /// What the chat is: `personal`, `group`, `channel`, `private-channel` or `thread`.
    pub const TYPE: &str = "chat.type";
    /// Who wrote in it, separated by `, `.
    pub const PARTICIPANTS: &str = "chat.participants";
    /// The time of its first message imported, `YYYY-MM-DDTHH:MM:SS` in the time of the
    /// export.
    pub const STARTED_AT: &str = "chat.started_at";
    /// The time of its last message imported.
    pub const ENDED_AT: &str = "chat.ended_at";
    /// How many messages were imported.
    pub const MESSAGES: &str = "chat.messages";
    /// Where the last import stopped: the position of its last message.
    pub const LAST: &str = "chat.last";
    /// The messages at that position, so one more there is still added.
    pub const LAST_KEYS: &str = "chat.last_keys";
    /// How a WhatsApp chat's dates were read: `day-month` or `month-day`. Kept from the first
    /// import, so later exports of the chat read their dates the same way.
    pub const DATE_ORDER: &str = "chat.date_order";
}

/// The kind of a chat export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ChatFormat {
    /// A WhatsApp chat export.
    WhatsApp,
    /// A Telegram Desktop export, in JSON.
    Telegram,
    /// A Slack workspace export.
    Slack,
    /// iMessage, read live from the Messages database of this Mac ([`imessage`]), not from an
    /// export.
    IMessage,
}

impl ChatFormat {
    /// Every export format.
    pub const ALL: [ChatFormat; 3] = [
        ChatFormat::WhatsApp,
        ChatFormat::Telegram,
        ChatFormat::Slack,
    ];

    /// Its id: `whatsapp`, `telegram`, `slack` or `imessage`.
    pub fn id(self) -> &'static str {
        match self {
            ChatFormat::WhatsApp => "whatsapp",
            ChatFormat::Telegram => "telegram",
            ChatFormat::Slack => "slack",
            ChatFormat::IMessage => imessage::APP,
        }
    }
}

impl fmt::Display for ChatFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ChatFormat::WhatsApp => "WhatsApp export",
            ChatFormat::Telegram => "Telegram Desktop export",
            ChatFormat::Slack => "Slack export",
            ChatFormat::IMessage => "iMessage database",
        })
    }
}

/// Why a chat import failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum ChatImportError {
    /// The export could not be read.
    Io(io::Error),
    /// A file of the export is not the JSON it should be.
    Json {
        /// The file, inside the export.
        file: String,
        /// What was wrong.
        error: serde_json::Error,
    },
    /// The path is not a chat export Knode reads, or not one of the format asked for.
    NotRecognized {
        /// The path.
        path: PathBuf,
        /// The format asked for, if one was.
        format: Option<ChatFormat>,
    },
    /// The export does not say the chat's name, which finds it again.
    NoChatName(PathBuf),
    /// The space could not be written.
    Space(knode_format::Error),
}

impl fmt::Display for ChatImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChatImportError::Io(error) => write!(f, "the export could not be read: {error}"),
            ChatImportError::Json { file, error } => write!(f, "{file} could not be read: {error}"),
            ChatImportError::NotRecognized { path, format } => match format {
                Some(format) => write!(f, "{} is not a {format}", path.display()),
                None => write!(
                    f,
                    "{} is not a WhatsApp, Telegram Desktop or Slack chat export",
                    path.display()
                ),
            },
            ChatImportError::NoChatName(path) => write!(
                f,
                "{}: the chat's name is not in the export's file names, and Knode finds a chat \
                 again by its name. Import the .zip as WhatsApp named it (WhatsApp Chat - \
                 <name>.zip), or put _chat.txt in a folder named so",
                path.display()
            ),
            ChatImportError::Space(error) => {
                write!(f, "the import could not write the space: {error}")
            }
        }
    }
}

impl std::error::Error for ChatImportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ChatImportError::Io(error) => Some(error),
            ChatImportError::Json { error, .. } => Some(error),
            ChatImportError::NotRecognized { .. } | ChatImportError::NoChatName(_) => None,
            ChatImportError::Space(error) => Some(error),
        }
    }
}

impl From<io::Error> for ChatImportError {
    fn from(error: io::Error) -> Self {
        ChatImportError::Io(error)
    }
}

impl From<knode_format::Error> for ChatImportError {
    fn from(error: knode_format::Error) -> Self {
        ChatImportError::Space(error)
    }
}

/// What [`import_chat_export`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatImportReport {
    /// The kind of export read.
    pub format: ChatFormat,
    /// The chat nodes created or given new messages, in the order they were written.
    pub chats: Vec<Imported>,
    /// Chats with no new messages.
    pub unchanged: usize,
    /// Chats with no messages at all; no node is written for them.
    pub empty: usize,
    /// Messages added, in all.
    pub messages: usize,
    /// Files attached.
    pub attached: usize,
    /// Files the chats name that are not in the export, or that it left out.
    pub missing: usize,
    /// The `person` nodes created for the members of a Slack workspace.
    pub people: Vec<Imported>,
    /// What could not be imported, such as a file too large to attach; the rest was.
    pub skipped: Vec<Skipped>,
    /// What else is worth knowing, such as dates that could be read two ways.
    pub notes: Vec<String>,
}

impl ChatImportReport {
    pub(crate) fn new(format: ChatFormat) -> Self {
        ChatImportReport {
            format,
            chats: Vec::new(),
            unchanged: 0,
            empty: 0,
            messages: 0,
            attached: 0,
            missing: 0,
            people: Vec::new(),
            skipped: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub(crate) fn skip(&mut self, item: impl Into<String>, reason: impl Into<String>) {
        self.skipped.push(Skipped {
            item: item.into(),
            reason: reason.into(),
        });
    }
}

/// What is in an export, found.
enum Located {
    WhatsApp(String),
    Telegram(String),
    Slack(String),
}

impl Located {
    fn format(&self) -> ChatFormat {
        match self {
            Located::WhatsApp(_) => ChatFormat::WhatsApp,
            Located::Telegram(_) => ChatFormat::Telegram,
            Located::Slack(_) => ChatFormat::Slack,
        }
    }
}

/// Says in the report how many secrets in the messages were replaced, if any.
pub(crate) fn note_redacted(report: &mut ChatImportReport, n: usize) {
    if n > 0 {
        report.notes.push(format!(
            "{n} {} in the messages (API keys, tokens, passwords) replaced with {}.",
            if n == 1 { "secret" } else { "secrets" },
            crate::REDACTED
        ));
    }
}

/// Finds the export in `files`. Told from the files alone (`format` is `None`), an export is
/// only one when it is what the person gave: its files at the top, or in the one folder at the
/// top, and no Markdown notes anywhere in it outside the export's own media folders. A folder of
/// notes that holds a chat export, wherever it is, is a folder of notes, not that chat. Named,
/// the format is looked for at any depth, the shallowest first.
fn locate(files: &mut Files, format: Option<ChatFormat>) -> Option<Located> {
    let root = match format {
        Some(_) => None,
        None => Some(files.root()),
    };
    let located = locate_in(files, format, root.as_deref())?;
    if let Some(root) = &root
        && files.single.is_none()
        && has_notes(files, root, located.format())
    {
        return None;
    }
    Some(located)
}

fn locate_in(files: &mut Files, format: Option<ChatFormat>, root: Option<&str>) -> Option<Located> {
    let wanted = |f: ChatFormat| format.is_none_or(|w| w == f);
    if wanted(ChatFormat::WhatsApp)
        && let Some(file) = whatsapp::locate(files, root)
    {
        return Some(Located::WhatsApp(file));
    }
    if wanted(ChatFormat::Telegram)
        && let Some(file) = telegram::locate(files, root)
    {
        return Some(Located::Telegram(file));
    }
    if wanted(ChatFormat::Slack)
        && let Some(dir) = slack::locate(files, root)
    {
        return Some(Located::Slack(dir));
    }
    None
}

/// The folders, in an export's own folder, where it keeps the files sent in its chats: a
/// Markdown file there was sent in a chat, it is not a note.
fn media_folders(format: ChatFormat) -> &'static [&'static str] {
    match format {
        // Media sit beside the chat's text.
        ChatFormat::WhatsApp => &[],
        ChatFormat::Telegram => &[
            "files",
            "photos",
            "video_files",
            "voice_messages",
            "round_video_messages",
            "audio_files",
            "stickers",
            "animations",
            "contacts",
            // An account export keeps each chat's media in `chats/chat_<n>/`.
            "chats",
        ],
        ChatFormat::Slack => &["__uploads"],
        // Not an export: Messages is read from its database.
        ChatFormat::IMessage => &[],
    }
}

/// Whether Markdown notes are anywhere in the files, outside the media folders an export of
/// `format` keeps in `root`.
fn has_notes(files: &Files, root: &str, format: ChatFormat) -> bool {
    let media: Vec<String> = media_folders(format)
        .iter()
        .map(|dir| format!("{}/", files::join(root, dir)))
        .collect();
    files.names().iter().any(|n| {
        let lower = file_name(n).to_ascii_lowercase();
        (lower.ends_with(".md") || lower.ends_with(".markdown"))
            && !media.iter().any(|dir| n.starts_with(dir.as_str()))
    })
}

/// A chat export somewhere in the folder or `.zip` at `path` that [`detect_chat_export`] does
/// not take as what `path` is, such as one inside a folder of notes: its format, and the path
/// to give [`import_chat_export`] with that format to import it (the `.zip` itself for one
/// inside a `.zip`).
pub fn chat_export_inside(path: impl AsRef<Path>) -> Option<(ChatFormat, PathBuf)> {
    let path = path.as_ref();
    let mut files = Files::open(path).ok()?;
    if files.single.is_some() {
        return None;
    }
    let located = ChatFormat::ALL
        .into_iter()
        .find_map(|format| locate_in(&mut files, Some(format), None))?;
    if !path.is_dir() {
        return Some((located.format(), path.to_path_buf()));
    }
    let inner = |name: &str| {
        let mut out = path.to_path_buf();
        out.extend(name.split('/').filter(|part| !part.is_empty()));
        out
    };
    Some(match located {
        Located::WhatsApp(file) => (ChatFormat::WhatsApp, inner(&file)),
        Located::Telegram(file) => (ChatFormat::Telegram, inner(files::parent(&file))),
        Located::Slack(dir) => (ChatFormat::Slack, inner(&dir)),
    })
}

/// Whether `name`, a path inside the export, may be where an export of `root` keeps its
/// files: anywhere when `root` is `None`.
fn in_root(name: &str, root: Option<&str>) -> bool {
    root.is_none_or(|root| files::parent(name) == root)
}

/// The kind of chat export at `path` (a `.zip`, a folder, or a chat's `.txt`), if it is one:
/// told from its files at the top (or in the one folder at the top). A folder with Markdown
/// notes in it is not a chat export, nor one that only holds an export further down: see
/// [`chat_export_inside`] for those.
pub fn detect_chat_export(path: impl AsRef<Path>) -> Option<ChatFormat> {
    let mut files = Files::open(path.as_ref()).ok()?;
    locate(&mut files, None).map(|located| located.format())
}

/// Whether `path` holds an export of `format`, looked for at any depth, as
/// [`import_chat_export`] does when it is given the format.
pub fn is_chat_export(path: impl AsRef<Path>, format: ChatFormat) -> bool {
    Files::open(path.as_ref())
        .ok()
        .is_some_and(|mut files| locate(&mut files, Some(format)).is_some())
}

/// Imports the chats of an export into `space`, written by `by` with `path` as the source:
/// one [`CHAT_KIND`] node per chat, its messages grouped by day, and the files of the export
/// attached to it. `format` names the kind of export; without it, it is told from the files.
///
/// A chat imported before is found again by its app and id ([`keys::APP`], [`keys::ID`]), and
/// only the messages after the last one imported then are added to its node, under the day
/// they belong to: importing the same export again changes nothing, and a newer export adds
/// what is new. A file named by a message but not in the export is marked as missing in the
/// text.
///
/// ```no_run
/// use knode_format::{Provenance, Space};
/// use knode_ingest::import_chat_export;
///
/// let mut space = Space::open("/Users/example/Knode/Personal.knode")?;
/// let report = import_chat_export(&mut space, "/Users/example/Downloads/WhatsApp Chat - Ada.zip",
///     None, &Provenance::person("example"))?;
/// println!("{}: {} messages added", report.format, report.messages);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn import_chat_export(
    space: &mut Space,
    path: impl AsRef<Path>,
    format: Option<ChatFormat>,
    by: &Provenance,
) -> Result<ChatImportReport, ChatImportError> {
    let path = path.as_ref();
    let not_recognized = || ChatImportError::NotRecognized {
        path: path.to_path_buf(),
        format,
    };
    let mut files = Files::open(path).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => ChatImportError::Io(e),
        _ => not_recognized(),
    })?;
    let located = locate(&mut files, format).ok_or_else(not_recognized)?;
    let format = located.format();
    let mut writer = ChatWriter {
        space,
        by: by.clone().with_source(path.to_string_lossy()),
        report: ChatImportReport::new(format),
    };
    match located {
        Located::WhatsApp(file) => {
            let name = whatsapp::name(&files, path, &file)
                .ok_or_else(|| ChatImportError::NoChatName(path.to_path_buf()))?;
            let stored = match writer.find_id(ChatFormat::WhatsApp, &name.to_lowercase())? {
                Some(id) => writer.space.property(&id, keys::DATE_ORDER)?,
                None => None,
            };
            match whatsapp::read(&mut files, &file, &name, stored.as_deref())? {
                Ok(read) => {
                    for (item, reason) in read.skipped {
                        writer.report.skip(item, reason);
                    }
                    writer.report.notes.extend(read.note);
                    note_redacted(&mut writer.report, read.redacted);
                    writer.write(&read.chat, &mut files)?;
                }
                Err(conflict) => {
                    writer.report.skip(format!("WhatsApp - {name}"), conflict.0);
                }
            }
        }
        Located::Telegram(file) => {
            // `result.json` is read from a second handle on the export, while its media are
            // attached from the first.
            let mut source = Files::open(path)?;
            telegram::import(&mut source, &file, &mut files, &mut writer)?;
        }
        Located::Slack(dir) => slack::import(&mut files, &dir, &mut writer)?,
    }
    Ok(writer.report)
}
