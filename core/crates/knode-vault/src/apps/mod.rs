//! Notes from other note apps: Apple Notes, Bear, Notion and Evernote, read into a space with
//! [`import_notes`].
//!
//! | App | What is read | Status |
//! | --- | --- | --- |
//! | Apple Notes | Its database, `NoteStore.sqlite`, read-only (macOS, needs Full Disk Access), or a folder of notes exported as Markdown | Note bodies are a format Apple does not document: text, headings, lists, checklists, quotes, monospaced blocks, bold, italic, strikethrough, links, hashtags, links between notes and attached files are read; tables, drawings and scans are not (each leaves a line saying so) |
//! | Bear | A backup (`.bear2bk`, `.bearbk`), TextBundles (`.textbundle`, `.textpack`, or a folder of them), its database (`database.sqlite`, read-only), or a folder of Markdown files | Tags (`#tag`, `#nested/tag`, `#multi word#`), `[[links]]` and files |
//! | Notion | The "Markdown & CSV" export, as downloaded (`.zip`, parts included) or extracted | Pages, nested pages, databases as tables with a node per row, files, links between pages |
//! | Evernote | `.enex` files, one per notebook, or a folder of them | Notebooks, tags, files (by their MD5 hash), checklists, tables, code blocks; encrypted text is left out |
//!
//! Every note becomes one `note` node: its title, its body in Markdown, its tags, and its
//! creation and change dates when the app keeps them (the node is written with
//! [`Space::import_node`](knode_format::Space::import_node)). Where the note was (folder,
//! notebook, parent pages) is kept in the property [`keys::FOLDER`], one path with `/` between
//! names. Files in a note are attached to its node and embedded as `![[name]]` (images) or
//! linked as `[[name]]`. Links between notes become `[[Title]]` links when the target is in the
//! same import; Notion sub-pages and database rows are also linked `part-of` their parent.
//!
//! Each node keeps the app ([`keys::APP`]) and the note's id in the app ([`keys::ID`]), so
//! importing again updates the same nodes instead of duplicating them, and a note that did not
//! change in the app since the last import ([`keys::HASH`]) is left as it is, edits made in
//! Knode included.

mod apple_notes;
mod bear;
mod dates;
mod db;
mod enml;
mod evernote;
mod folder;
mod markdown;
mod notion;
mod proto;
mod source;
mod write;

use std::fmt;
use std::path::{Path, PathBuf};

use knode_format::{NodeId, Provenance, Space};
use serde::{Deserialize, Serialize};

pub use crate::Skipped;

/// The node properties the note importers write.
pub mod keys {
    /// The app the note came from: `apple-notes`, `bear`, `notion` or `evernote`.
    pub const APP: &str = "import.app";
    /// The note's id in the app, which finds the node again on the next import.
    pub const ID: &str = "import.id";
    /// Where the note was in the app: its folder, notebook or parent pages, with `/` between
    /// names.
    pub const FOLDER: &str = "import.folder";
    /// A hash of the note as last imported: while the app's note still has it, importing again
    /// leaves the node as it is.
    pub const HASH: &str = "import.hash";
    /// The web address the note was clipped from, when the app keeps it.
    pub const URL: &str = "import.url";
    /// The author the app names for the note.
    pub const AUTHOR: &str = "import.author";
    /// `true` for a note archived in the app.
    pub const ARCHIVED: &str = "import.archived";
    /// The prefix of the properties a Notion database row gets from its columns:
    /// `notion.status`, `notion.due-date`.
    pub const NOTION_PREFIX: &str = "notion.";
}

/// A note app [`import_notes`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum NoteApp {
    /// Apple Notes.
    AppleNotes,
    /// Bear.
    Bear,
    /// Notion.
    Notion,
    /// Evernote.
    Evernote,
}

impl NoteApp {
    /// Every app, in the order they are listed.
    pub const ALL: [NoteApp; 4] = [
        NoteApp::AppleNotes,
        NoteApp::Bear,
        NoteApp::Notion,
        NoteApp::Evernote,
    ];

    /// The id kept in [`keys::APP`]: `apple-notes`, `bear`, `notion`, `evernote`.
    pub fn id(self) -> &'static str {
        match self {
            NoteApp::AppleNotes => "apple-notes",
            NoteApp::Bear => "bear",
            NoteApp::Notion => "notion",
            NoteApp::Evernote => "evernote",
        }
    }

    /// The app with this [`id`](Self::id).
    pub fn from_id(id: &str) -> Option<NoteApp> {
        NoteApp::ALL.into_iter().find(|app| app.id() == id)
    }

    /// Where the app keeps its notes on this Mac, when Knode can read them from there: the
    /// Apple Notes group container and Bear's database folder. `None` for Notion and Evernote,
    /// which are read from their exports, and off macOS.
    pub fn default_location(self) -> Option<PathBuf> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let home = PathBuf::from(std::env::var_os("HOME")?);
        let containers = home.join("Library").join("Group Containers");
        match self {
            NoteApp::AppleNotes => Some(containers.join(apple_notes::CONTAINER)),
            NoteApp::Bear => Some(containers.join(bear::CONTAINER).join("Application Data")),
            NoteApp::Notion | NoteApp::Evernote => None,
        }
    }
}

impl fmt::Display for NoteApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            NoteApp::AppleNotes => "Apple Notes",
            NoteApp::Bear => "Bear",
            NoteApp::Notion => "Notion",
            NoteApp::Evernote => "Evernote",
        })
    }
}

/// Why [`import_notes`] could not read the notes. Notes that cannot be read one by one do not
/// fail the import: they are listed in [`NotesReport::skipped`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ImportError {
    /// macOS keeps Knode from reading the app's notes: the app running Knode needs Full Disk
    /// Access.
    #[error(
        "macOS does not let Knode read {} ({app} keeps its notes there): only apps with Full \
         Disk Access can. Open System Settings > Privacy & Security > Full Disk Access, turn it \
         on for the app that runs Knode (Knode, or your terminal), then quit and reopen that \
         app. Or export the notes from {app} and import the exported files.",
        path.display()
    )]
    NoAccess {
        /// The app.
        app: NoteApp,
        /// The folder or file that could not be read.
        path: PathBuf,
    },
    /// Nothing at the path.
    #[error("{} does not exist", .0.display())]
    NotFound(PathBuf),
    /// The path is not something Knode reads for this app.
    #[error("{} is not {what}", path.display())]
    NotRecognized {
        /// The path.
        path: PathBuf,
        /// What it should have been.
        what: String,
    },
    /// No app could be told from the path.
    #[error(
        "{} is not a note export Knode recognizes: name the app (Apple Notes, Bear, Notion or Evernote)",
        .0.display()
    )]
    UnknownApp(PathBuf),
    /// A file could not be read.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        source: std::io::Error,
    },
    /// The app's file is not what it should be.
    #[error("{}: {message}", path.display())]
    Unreadable {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        message: String,
    },
    /// The space returned an error.
    #[error(transparent)]
    Space(#[from] knode_format::Error),
}

pub(crate) fn io_error(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> ImportError {
    let path = path.into();
    move |source| ImportError::Io { path, source }
}

/// Fails with [`ImportError::NoAccess`] when macOS keeps the path from being read, as it does
/// for other apps' containers without Full Disk Access.
fn check_access(app: NoteApp, path: &Path) -> Result<(), ImportError> {
    let denied = |e: std::io::Error| e.kind() == std::io::ErrorKind::PermissionDenied;
    let blocked = match std::fs::metadata(path) {
        Err(e) => denied(e),
        Ok(meta) if meta.is_dir() => {
            std::fs::read_dir(path).err().is_some_and(denied)
                || [apple_notes::DATABASE, bear::DATABASE]
                    .iter()
                    .map(|name| path.join(name))
                    .any(|file| std::fs::File::open(file).err().is_some_and(denied))
        }
        Ok(_) => std::fs::File::open(path).err().is_some_and(denied),
    };
    if blocked {
        return Err(ImportError::NoAccess {
            app,
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

/// One node [`import_notes`] created or changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedNote {
    /// The node.
    pub node_id: NodeId,
    /// The note's id in the app ([`keys::ID`]).
    pub source_id: String,
    /// Its title.
    pub title: String,
}

/// What [`import_notes`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotesReport {
    /// The app read.
    pub app: NoteApp,
    /// The nodes created.
    pub created: Vec<ImportedNote>,
    /// The nodes changed: the note changed in the app since the last import.
    pub updated: Vec<ImportedNote>,
    /// How many notes had not changed since the last import.
    pub unchanged: usize,
    /// How many files were attached.
    pub attached: usize,
    /// How many `part-of` links were made or already existed.
    pub links: usize,
    /// The notes and files that could not be imported, and why. `path` is the note's title or
    /// file.
    pub skipped: Vec<Skipped>,
    /// What the import read only in part, such as Apple Notes tables, one line each.
    pub limits: Vec<String>,
}

impl NotesReport {
    fn new(app: NoteApp) -> Self {
        Self {
            app,
            created: Vec::new(),
            updated: Vec::new(),
            unchanged: 0,
            attached: 0,
            links: 0,
            skipped: Vec::new(),
            limits: Vec::new(),
        }
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

/// The app whose notes are at `path`, when it can be told from the path alone:
///
/// - Apple Notes: its group container, or a `NoteStore.sqlite`;
/// - Bear: its database folder or `database.sqlite`, a `.bear2bk` or `.bearbk` backup, a
///   `.textbundle` or `.textpack`, or a folder of TextBundles;
/// - Notion: a `.zip` or folder whose files end with Notion's 32-character page ids;
/// - Evernote: an `.enex` file, or a folder or `.zip` of them.
///
/// A plain folder of Markdown files could come from many apps: `None`.
pub fn detect(path: impl AsRef<Path>) -> Option<NoteApp> {
    let path = path.as_ref();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if name == apple_notes::DATABASE.to_lowercase() || name == apple_notes::CONTAINER {
        return Some(NoteApp::AppleNotes);
    }
    if matches!(
        ext.as_str(),
        "bear2bk" | "bearbk" | "textbundle" | "textpack"
    ) || name == bear::CONTAINER.to_lowercase()
    {
        return Some(NoteApp::Bear);
    }
    if ext == "enex" {
        return Some(NoteApp::Evernote);
    }
    if name == bear::DATABASE && bear::is_database(path) {
        return Some(NoteApp::Bear);
    }
    if path.is_dir() {
        if path.join(apple_notes::DATABASE).is_file() {
            return Some(NoteApp::AppleNotes);
        }
        if path.join(bear::DATABASE).is_file() && bear::is_database(&path.join(bear::DATABASE)) {
            return Some(NoteApp::Bear);
        }
    }
    let names = source::Source::open(path).ok()?.names().to_vec();
    if names.iter().any(|n| n.to_lowercase().ends_with(".enex")) {
        return Some(NoteApp::Evernote);
    }
    if names.iter().any(|n| bear::in_bundle(n)) {
        return Some(NoteApp::Bear);
    }
    if names.iter().any(|n| notion::is_notion_name(n)) {
        return Some(NoteApp::Notion);
    }
    None
}

/// Imports the notes of `app` at `path` into `space`, written by `by` (with `path` as the
/// [`source`](Provenance::source) when `by` has none).
///
/// `path` is what [`detect`] lists for the app, or for Apple Notes and Bear a folder of notes
/// they exported as Markdown, or for Apple Notes and Bear the folder of
/// [`NoteApp::default_location`]. The app's own files are only ever read; a database is copied
/// and the copy read, so an app that is running is not disturbed.
///
/// Notes in the trash, deleted notes and locked or encrypted notes are not imported (the locked
/// ones are listed in [`NotesReport::skipped`]). See the [module](self) for what each app
/// gives.
///
/// ```no_run
/// use knode_format::{Provenance, Space};
/// use knode_vault::apps::{NoteApp, import_notes};
///
/// let mut space = Space::open("/Users/example/Knode/Personal.knode")?;
/// let report = import_notes(&mut space, NoteApp::Evernote, "/Users/example/Downloads/Work.enex",
///     &Provenance::person("example"))?;
/// println!("{} new notes", report.created.len());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn import_notes(
    space: &mut Space,
    app: NoteApp,
    path: impl AsRef<Path>,
    by: &Provenance,
) -> Result<NotesReport, ImportError> {
    let path = path.as_ref();
    check_access(app, path)?;
    if !path.exists() {
        return Err(ImportError::NotFound(path.to_path_buf()));
    }
    let by = if by.source.is_some() {
        by.clone()
    } else {
        by.clone().with_source(path.to_string_lossy())
    };
    let mut report = NotesReport::new(app);
    let batch = match app {
        NoteApp::AppleNotes => apple_notes::read(path, &mut report)?,
        NoteApp::Bear => bear::read(path, &mut report)?,
        NoteApp::Notion => notion::read(path, &mut report)?,
        NoteApp::Evernote => evernote::read(path, &mut report)?,
    };
    write::write(space, app, batch, &by, &mut report)?;
    Ok(report)
}
