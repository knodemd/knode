//! Importing chat exports into a space: [`import_chat_export`].
//!
//! | Export | Format id | What it reads |
//! | --- | --- | --- |
//! | WhatsApp "Export chat" | `whatsapp` | The `.zip`, its folder, or the `.txt`: `_chat.txt` (iOS) or `WhatsApp Chat with <name>.txt` (Android), in every locale, with the media beside it |
//! | Telegram Desktop, JSON | `telegram` | The folder with `result.json` (one chat, or the whole account), or a `.zip` of it, with the media beside it |
//!
//! Every chat becomes one node of kind [`CHAT_KIND`]: the messages grouped by day under
//! `## YYYY-MM-DD` headings, each with its sender and time, and the files of the export
//! attached to the node. Its properties ([`keys`]) say which app and chat it is, who wrote in
//! it and when. Importing the same chat again, or a newer export of it, adds only the messages
//! after the last one imported: none is written twice.
//!
//! Everything is read from the files the person gives and written only into the space they
//! pick; nothing is sent anywhere.

mod files;
mod model;
mod render;
mod telegram;
mod whatsapp;
mod write;

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use knode_format::{Provenance, Space};

use crate::exports::{Imported, Skipped};
use files::Files;
use write::ChatWriter;

/// The kind of the node of a chat.
pub const CHAT_KIND: &str = "chat";

/// The properties of a chat's node.
pub mod keys {
    /// The app: `whatsapp` or `telegram`.
    pub const APP: &str = "chat.app";
    /// The chat's id in the app, or, for apps that have none, one made from its name.
    pub const ID: &str = "chat.id";
    /// What the chat is: `personal`, `group` or `channel`.
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
}

impl ChatFormat {
    /// Every format.
    pub const ALL: [ChatFormat; 2] = [ChatFormat::WhatsApp, ChatFormat::Telegram];

    /// Its id: `whatsapp` or `telegram`.
    pub fn id(self) -> &'static str {
        match self {
            ChatFormat::WhatsApp => "whatsapp",
            ChatFormat::Telegram => "telegram",
        }
    }
}

impl fmt::Display for ChatFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ChatFormat::WhatsApp => "WhatsApp export",
            ChatFormat::Telegram => "Telegram Desktop export",
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
                    "{} is not a WhatsApp or Telegram Desktop chat export",
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
    /// What could not be imported, such as a file too large to attach; the rest was.
    pub skipped: Vec<Skipped>,
    /// What else is worth knowing, such as dates that could be read two ways.
    pub notes: Vec<String>,
}

impl ChatImportReport {
    fn new(format: ChatFormat) -> Self {
        ChatImportReport {
            format,
            chats: Vec::new(),
            unchanged: 0,
            empty: 0,
            messages: 0,
            attached: 0,
            missing: 0,
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
}

impl Located {
    fn format(&self) -> ChatFormat {
        match self {
            Located::WhatsApp(_) => ChatFormat::WhatsApp,
            Located::Telegram(_) => ChatFormat::Telegram,
        }
    }
}

fn locate(files: &mut Files, format: Option<ChatFormat>) -> Option<Located> {
    let wanted = |f: ChatFormat| format.is_none_or(|w| w == f);
    if wanted(ChatFormat::WhatsApp)
        && let Some(file) = whatsapp::locate(files)
    {
        return Some(Located::WhatsApp(file));
    }
    if wanted(ChatFormat::Telegram)
        && let Some(file) = telegram::locate(files)
    {
        return Some(Located::Telegram(file));
    }
    None
}

/// The kind of chat export at `path` (a `.zip`, a folder, or a chat's `.txt`), if it is one.
pub fn detect_chat_export(path: impl AsRef<Path>) -> Option<ChatFormat> {
    let mut files = Files::open(path.as_ref()).ok()?;
    locate(&mut files, None).map(|located| located.format())
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
                    writer.write(&read.chat, &mut files)?;
                }
                Err(conflict) => {
                    writer.report.skip(format!("WhatsApp - {name}"), conflict.0);
                }
            }
        }
        Located::Telegram(file) => {
            for chat in telegram::read(&mut files, &file)? {
                writer.write(&chat, &mut files)?;
            }
        }
    }
    Ok(writer.report)
}
