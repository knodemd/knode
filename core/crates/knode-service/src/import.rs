//! `import.run`: everything a person brings into a space, from one path or URL.
//!
//! [`prepare`] tells what is at a path (or takes the kind the person names), without opening
//! the space; [`run`] then imports it and returns one [`ImportResult`] whatever the source:
//!
//! - data exports of Claude, ChatGPT and Gemini (Google Takeout), with [`knode_ingest`];
//! - Markdown: a folder (an export or an Obsidian vault) or one `.md` file;
//! - a Claude Code or Codex session file (`.jsonl`);
//! - reading, with [`knode_vault::reading`]: a PDF, a saved web page, a web page fetched from
//!   an `http` or `https` URL the person gave, Kindle clippings or a Kindle notebook, and
//!   Readwise CSV and Markdown exports;
//! - notes from other apps, with [`knode_vault::apps`]: Apple Notes, Bear, Notion and Evernote.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use knode_format::{NewNode, NodeId, NodeUpdate, Provenance, Space, kinds};
use knode_ingest::{Agent, SessionReader, to_markdown};
use knode_vault::apps::{self, NoteApp};
use knode_vault::reading::{self, Outcome, ReadingReport};
use serde::{Deserialize, Serialize};

use crate::maps::SkippedItem;

/// The kinds of source `import.run` reads, as `from` names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImportSource {
    /// A Claude, ChatGPT or Gemini (Google Takeout) data export: the `.zip` or its folder.
    Export,
    /// A folder of Markdown files (an export or an Obsidian vault), or one `.md` file.
    Markdown,
    /// A Claude Code or Codex session file.
    Session,
    /// A PDF.
    Pdf,
    /// A web page: saved as `.html`, or fetched from an `http` or `https` URL.
    Web,
    /// Kindle highlights: `My Clippings.txt` or a notebook exported as HTML.
    Kindle,
    /// A Readwise export: the CSV, or the Markdown (a `.md` file or the unzipped folder).
    Readwise,
    /// Apple Notes: its database, or notes it exported as Markdown.
    AppleNotes,
    /// Bear: a backup, TextBundles, its database, or notes it exported as Markdown.
    Bear,
    /// Notion: the "Markdown & CSV" export, zipped or extracted.
    Notion,
    /// Evernote: `.enex` files, or a folder of them.
    Evernote,
}

impl ImportSource {
    /// Every source, in the order they are listed.
    pub const ALL: [ImportSource; 11] = [
        ImportSource::Export,
        ImportSource::Markdown,
        ImportSource::Session,
        ImportSource::Pdf,
        ImportSource::Web,
        ImportSource::Kindle,
        ImportSource::Readwise,
        ImportSource::AppleNotes,
        ImportSource::Bear,
        ImportSource::Notion,
        ImportSource::Evernote,
    ];

    /// The name `from` takes: `export`, `markdown`, `session`, `pdf`, `web`, `kindle`,
    /// `readwise`, `apple-notes`, `bear`, `notion`, `evernote`.
    pub fn id(self) -> &'static str {
        match self {
            ImportSource::Export => "export",
            ImportSource::Markdown => "markdown",
            ImportSource::Session => "session",
            ImportSource::Pdf => "pdf",
            ImportSource::Web => "web",
            ImportSource::Kindle => "kindle",
            ImportSource::Readwise => "readwise",
            ImportSource::AppleNotes => "apple-notes",
            ImportSource::Bear => "bear",
            ImportSource::Notion => "notion",
            ImportSource::Evernote => "evernote",
        }
    }

    /// The source with this [`id`](Self::id).
    pub fn from_id(id: &str) -> Option<ImportSource> {
        let id = id.trim().to_ascii_lowercase().replace('_', "-");
        ImportSource::ALL.into_iter().find(|s| s.id() == id)
    }

    fn app(self) -> Option<NoteApp> {
        match self {
            ImportSource::AppleNotes => Some(NoteApp::AppleNotes),
            ImportSource::Bear => Some(NoteApp::Bear),
            ImportSource::Notion => Some(NoteApp::Notion),
            ImportSource::Evernote => Some(NoteApp::Evernote),
            _ => None,
        }
    }

    fn of_app(app: NoteApp) -> ImportSource {
        match app {
            NoteApp::AppleNotes => ImportSource::AppleNotes,
            NoteApp::Bear => ImportSource::Bear,
            NoteApp::Notion => ImportSource::Notion,
            _ => ImportSource::Evernote,
        }
    }
}

/// What happened to one node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportOutcome {
    /// The node is new.
    Created,
    /// The node changed.
    Updated,
    /// The node already had everything.
    Unchanged,
    /// The source changed, but so did the node, in Knode, since it was imported: it was left as
    /// it is.
    Edited,
}

/// One node an import created or changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedNode {
    /// The node, when the importer names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<NodeId>,
    /// Its title, or the file it came from.
    pub title: String,
    /// What happened to it.
    pub outcome: ImportOutcome,
    /// How many highlights were added to it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub highlights: usize,
    /// Something worth knowing about it, such as a PDF without text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// What `import.run` did, whatever it read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportResult {
    /// The kind of source read.
    pub source: ImportSource,
    /// What it was, for people: `Claude export`, `PDF`, `Kindle clippings`, `Bear`...
    pub format: String,
    /// The path or URL read.
    pub from: String,
    /// Nodes created.
    pub created: usize,
    /// Nodes changed.
    pub updated: usize,
    /// Nodes that were already up to date.
    pub unchanged: usize,
    /// Nodes left as they are because they were edited in Knode since the last import.
    #[serde(default)]
    pub edited: usize,
    /// Highlights added, in all.
    #[serde(default)]
    pub highlights: usize,
    /// Files attached to nodes.
    #[serde(default)]
    pub attached: usize,
    /// The nodes created or changed (and, for reading, every document read), in order.
    #[serde(default)]
    pub nodes: Vec<ImportedNode>,
    /// What could not be imported, and why; the rest was.
    #[serde(default)]
    pub skipped: Vec<SkippedItem>,
    /// What else is worth knowing: what was left out or read only in part.
    #[serde(default)]
    pub notes: Vec<String>,
}

impl ImportResult {
    fn new(source: ImportSource, format: impl Into<String>, from: impl Into<String>) -> Self {
        ImportResult {
            source,
            format: format.into(),
            from: from.into(),
            created: 0,
            updated: 0,
            unchanged: 0,
            edited: 0,
            highlights: 0,
            attached: 0,
            nodes: Vec::new(),
            skipped: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// How many nodes were read, written or not: nothing read means nothing was imported.
    pub fn read(&self) -> usize {
        self.created + self.updated + self.unchanged + self.edited
    }

    fn node(&mut self, id: Option<NodeId>, title: impl Into<String>, outcome: ImportOutcome) {
        match outcome {
            ImportOutcome::Created => self.created += 1,
            ImportOutcome::Updated => self.updated += 1,
            ImportOutcome::Unchanged => self.unchanged += 1,
            ImportOutcome::Edited => self.edited += 1,
        }
        self.nodes.push(ImportedNode {
            id,
            title: title.into(),
            outcome,
            highlights: 0,
            note: None,
        });
    }

    fn skip(&mut self, item: impl Into<String>, reason: impl Into<String>) {
        self.skipped.push(SkippedItem {
            item: item.into(),
            reason: reason.into(),
        });
    }
}

/// Why an import could not run. What fails item by item does not fail the import: it is in
/// [`ImportResult::skipped`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ImportError {
    /// Nothing at the path.
    #[error("{} does not exist", .0.display())]
    NotFound(PathBuf),
    /// What is at the path is not something Knode imports, or not the kind named.
    #[error("{0}")]
    Unknown(String),
    /// macOS keeps Knode from reading another app's notes: the message says how to allow it.
    #[error("{0}")]
    NoAccess(String),
    /// The files could not be read, or a page could not be fetched.
    #[error("{0}")]
    Unreadable(String),
    /// The space returned an error.
    #[error(transparent)]
    Space(#[from] knode_format::Error),
}

/// The params of `import.run`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ImportRunParams {
    pub space: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<ImportSource>,
    pub by: Provenance,
}

/// The sources `import.run` reads, for messages.
pub const SOURCES: &str = "a Claude, ChatGPT or Gemini data export, a folder of Markdown files, \
     a .md file, a Claude Code or Codex session (.jsonl), a PDF, a saved web page (.html), an \
     http(s) URL, Kindle's My Clippings.txt or notebook, a Readwise CSV or Markdown export, an \
     Apple Notes or Bear database, a Bear backup, a Notion export or an Evernote .enex file";

/// What [`prepare`] found: what to read and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    input: Input,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Input {
    Export(PathBuf),
    MarkdownFolder(PathBuf),
    MarkdownFile(PathBuf),
    Session(PathBuf),
    Pdf(PathBuf),
    Html(PathBuf),
    Url(String),
    KindleClippings(PathBuf),
    KindleNotebook(PathBuf),
    ReadwiseCsv(PathBuf),
    ReadwiseMarkdown(PathBuf),
    App(NoteApp, PathBuf),
}

impl Prepared {
    /// The kind of source found.
    pub fn source(&self) -> ImportSource {
        match &self.input {
            Input::Export(_) => ImportSource::Export,
            Input::MarkdownFolder(_) | Input::MarkdownFile(_) => ImportSource::Markdown,
            Input::Session(_) => ImportSource::Session,
            Input::Pdf(_) => ImportSource::Pdf,
            Input::Html(_) | Input::Url(_) => ImportSource::Web,
            Input::KindleClippings(_) | Input::KindleNotebook(_) => ImportSource::Kindle,
            Input::ReadwiseCsv(_) | Input::ReadwiseMarkdown(_) => ImportSource::Readwise,
            Input::App(app, _) => ImportSource::of_app(*app),
        }
    }

    /// What it is, for people.
    pub fn format(&self) -> String {
        match &self.input {
            Input::Export(path) => knode_ingest::detect_export(path)
                .map(|f| f.to_string())
                .unwrap_or_else(|_| "data export".into()),
            Input::MarkdownFolder(_) => "folder of Markdown files".into(),
            Input::MarkdownFile(_) => "Markdown file".into(),
            Input::Session(_) => "agent session".into(),
            Input::Pdf(_) => "PDF".into(),
            Input::Html(_) => "saved web page".into(),
            Input::Url(_) => "web page".into(),
            Input::KindleClippings(_) => "Kindle clippings".into(),
            Input::KindleNotebook(_) => "Kindle notebook".into(),
            Input::ReadwiseCsv(_) => "Readwise CSV export".into(),
            Input::ReadwiseMarkdown(_) => "Readwise Markdown export".into(),
            Input::App(app, _) => match app {
                NoteApp::AppleNotes => "Apple Notes".into(),
                NoteApp::Bear => "Bear".into(),
                NoteApp::Notion => "Notion export".into(),
                _ => "Evernote export".into(),
            },
        }
    }

    /// The path or URL to read.
    pub fn from(&self) -> String {
        match &self.input {
            Input::Url(url) => url.clone(),
            Input::Export(p)
            | Input::MarkdownFolder(p)
            | Input::MarkdownFile(p)
            | Input::Session(p)
            | Input::Pdf(p)
            | Input::Html(p)
            | Input::KindleClippings(p)
            | Input::KindleNotebook(p)
            | Input::ReadwiseCsv(p)
            | Input::ReadwiseMarkdown(p)
            | Input::App(_, p) => p.display().to_string(),
        }
    }
}

/// Whether `text` is a URL Knode fetches: `http` or `https`.
pub fn is_url(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Tells what to import: the page at `url` (only `http` and `https`, fetched when [`run`]
/// runs), or what is at `path`, as `from` names it or as found from the path and its content.
/// For Apple Notes and Bear, no path means the app's own notes on this Mac. `path` must be
/// absolute.
pub fn prepare(
    path: Option<&Path>,
    url: Option<&str>,
    from: Option<ImportSource>,
) -> Result<Prepared, ImportError> {
    let input = match (path, url) {
        (Some(_), Some(_)) => {
            return Err(ImportError::Unknown(
                "give a path or a URL, not both".into(),
            ));
        }
        (None, Some(url)) => {
            if !is_url(url) {
                return Err(ImportError::Unknown(format!(
                    "only http and https pages can be fetched: {url}"
                )));
            }
            match from {
                None | Some(ImportSource::Web) => Input::Url(url.trim().to_string()),
                Some(other) => {
                    return Err(ImportError::Unknown(format!(
                        "a URL is a web page: it cannot be imported as {}",
                        other.id()
                    )));
                }
            }
        }
        (None, None) => match from.and_then(ImportSource::app) {
            Some(app @ (NoteApp::AppleNotes | NoteApp::Bear)) => {
                let Some(location) = app.default_location() else {
                    return Err(ImportError::Unknown(format!(
                        "Knode reads the notes of {} on a Mac only: export them and import the \
                         exported files",
                        ImportSource::of_app(app).id()
                    )));
                };
                Input::App(app, location)
            }
            _ => {
                return Err(ImportError::Unknown(
                    "name what to import: a path or an http(s) URL".into(),
                ));
            }
        },
        (Some(path), None) => {
            if !path.is_absolute() {
                return Err(ImportError::Unknown(format!(
                    "path must be an absolute path: {}",
                    path.display()
                )));
            }
            if !path.exists() {
                return Err(ImportError::NotFound(path.to_path_buf()));
            }
            match from {
                Some(source) => named(source, path)?,
                None => detect(path)?,
            }
        }
    };
    Ok(Prepared { input })
}

/// What `path` is read as when the person names the kind.
fn named(source: ImportSource, path: &Path) -> Result<Input, ImportError> {
    let p = path.to_path_buf();
    let ext = extension(path);
    let not = |what: &str| {
        Err(ImportError::Unknown(format!(
            "{} is not {what}",
            path.display()
        )))
    };
    Ok(match source {
        ImportSource::Export => match knode_ingest::detect_export(path) {
            Ok(_) => Input::Export(p),
            Err(_) => return not("a Claude, ChatGPT or Gemini data export"),
        },
        ImportSource::Markdown if path.is_dir() => Input::MarkdownFolder(p),
        ImportSource::Markdown if matches!(ext.as_deref(), Some("md" | "markdown")) => {
            Input::MarkdownFile(p)
        }
        ImportSource::Markdown => return not("a folder of Markdown files or a .md file"),
        ImportSource::Session if path.is_file() => Input::Session(p),
        ImportSource::Session => return not("a session file"),
        ImportSource::Pdf if path.is_file() => Input::Pdf(p),
        ImportSource::Pdf => return not("a PDF file"),
        ImportSource::Web if path.is_file() => Input::Html(p),
        ImportSource::Web => return not("a saved web page (.html)"),
        ImportSource::Kindle if !path.is_file() => {
            return not("My Clippings.txt or a Kindle notebook (.html)");
        }
        ImportSource::Kindle if matches!(ext.as_deref(), Some("html" | "htm")) => {
            Input::KindleNotebook(p)
        }
        ImportSource::Kindle => Input::KindleClippings(p),
        ImportSource::Readwise if path.is_file() && ext.as_deref() == Some("csv") => {
            Input::ReadwiseCsv(p)
        }
        ImportSource::Readwise => Input::ReadwiseMarkdown(p),
        ImportSource::AppleNotes
        | ImportSource::Bear
        | ImportSource::Notion
        | ImportSource::Evernote => Input::App(source.app().expect("a note app"), p),
    })
}

/// What `path` is, from its name and content.
fn detect(path: &Path) -> Result<Input, ImportError> {
    let p = path.to_path_buf();
    if path.is_file() {
        match extension(path).as_deref() {
            Some("pdf") => return Ok(Input::Pdf(p)),
            Some("html" | "htm") => {
                let head = read_start(path, 2 * 1024 * 1024);
                return Ok(if head.contains("bookTitle") && head.contains("noteText") {
                    Input::KindleNotebook(p)
                } else {
                    Input::Html(p)
                });
            }
            Some("jsonl") => return Ok(Input::Session(p)),
            Some("csv") if is_readwise_csv(path) => return Ok(Input::ReadwiseCsv(p)),
            Some("txt") if is_kindle_clippings(path) => return Ok(Input::KindleClippings(p)),
            Some("md" | "markdown") => {
                return Ok(if is_readwise_markdown(&read_start(path, 256 * 1024)) {
                    Input::ReadwiseMarkdown(p)
                } else {
                    Input::MarkdownFile(p)
                });
            }
            _ => {}
        }
    }
    if knode_ingest::detect_export(path).is_ok() {
        return Ok(Input::Export(p));
    }
    if let Some(app) = apps::detect(path) {
        return Ok(Input::App(app, p));
    }
    if path.is_dir() {
        return Ok(if is_readwise_folder(path) {
            Input::ReadwiseMarkdown(p)
        } else {
            Input::MarkdownFolder(p)
        });
    }
    Err(ImportError::Unknown(format!(
        "{} is not something Knode imports: it takes {SOURCES}",
        path.display()
    )))
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// The first `limit` bytes of a file, as text.
fn read_start(path: &Path, limit: u64) -> String {
    let mut bytes = Vec::new();
    if let Ok(file) = fs::File::open(path) {
        let _ = file.take(limit).read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A Readwise CSV export: its header names the highlight and the book.
fn is_readwise_csv(path: &Path) -> bool {
    let Ok(file) = fs::File::open(path) else {
        return false;
    };
    let mut header = String::new();
    if BufReader::new(file).read_line(&mut header).is_err() {
        return false;
    }
    let header = header.trim_start_matches('\u{feff}');
    header.contains("Highlight") && header.contains("Book Title")
}

/// `My Clippings.txt`, by its name or by the separators between clippings.
fn is_kindle_clippings(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name.contains("clippings")
        || read_start(path, 64 * 1024)
            .lines()
            .any(|l| l.trim_end() == "==========")
}

/// A file of the Readwise Markdown export: a `## Metadata` and a `## Highlights` section.
fn is_readwise_markdown(text: &str) -> bool {
    let mut metadata = false;
    let mut highlights = false;
    for line in text.lines() {
        match line.trim_end() {
            "## Metadata" => metadata = true,
            "## Highlights" => highlights = true,
            _ => {}
        }
    }
    metadata && highlights
}

/// The unzipped Readwise Markdown export: every `.md` file in it is a Readwise file.
fn is_readwise_folder(dir: &Path) -> bool {
    let mut files = Vec::new();
    collect_md(dir, 0, &mut files);
    !files.is_empty()
        && files
            .iter()
            .all(|f| is_readwise_markdown(&read_start(f, 256 * 1024)))
}

fn collect_md(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 3 || out.len() >= 200 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect_md(&path, depth + 1, out);
        } else if matches!(extension(&path).as_deref(), Some("md" | "markdown")) {
            out.push(path);
        }
    }
}

/// Imports what [`prepare`] found into `space`, written by `by` (a session, by its agent).
pub fn run(
    space: &mut Space,
    what: &Prepared,
    by: &Provenance,
) -> Result<ImportResult, ImportError> {
    let mut result = ImportResult::new(what.source(), what.format(), what.from());
    match &what.input {
        Input::Export(path) => {
            let report = knode_ingest::import_export(space, path, by).map_err(ingest_error)?;
            result.format = report.format.to_string();
            for c in report.conversations.iter().chain(&report.projects) {
                let outcome = match c.op {
                    knode_format::ChangeOp::Create => ImportOutcome::Created,
                    _ => ImportOutcome::Updated,
                };
                result.node(Some(c.node_id), c.title.clone(), outcome);
            }
            result.unchanged += report.unchanged;
            for s in &report.skipped {
                result.skip(s.item.clone(), s.reason.clone());
            }
            if !report.projects.is_empty() {
                result.notes.push(format!(
                    "{} written or changed",
                    plural(report.projects.len(), "project")
                ));
            }
            if report.empty > 0 {
                result.notes.push(format!(
                    "{} with no messages left out",
                    plural(report.empty, "conversation")
                ));
            }
        }
        Input::MarkdownFolder(dir) => {
            let report = knode_vault::import(space, dir, by).map_err(vault_error)?;
            for file in &report.created {
                result.node(None, file.clone(), ImportOutcome::Created);
            }
            for file in &report.updated {
                result.node(None, file.clone(), ImportOutcome::Updated);
            }
            result.unchanged += report.unchanged;
            result.attached = report.attached.len();
            for s in report.skipped.iter().chain(&report.unlinked) {
                result.skip(s.path.clone(), s.reason.clone());
            }
            if !report.attachments.is_empty() {
                let n = report.attachments.len();
                result.notes.push(format!(
                    "Left out {} that {} not Markdown, such as images.",
                    plural(n, "file"),
                    if n == 1 { "is" } else { "are" }
                ));
            }
            if report.created.len() + report.updated.len() + report.unchanged == 0
                && report.skipped.is_empty()
            {
                return Err(ImportError::Unknown(format!(
                    "{} has no Markdown (.md) files",
                    dir.display()
                )));
            }
        }
        Input::MarkdownFile(file) => markdown_file(space, file, by, &mut result)?,
        Input::Session(file) => session(space, file, &mut result)?,
        Input::Pdf(path) => reading_result(reading::import_pdf(space, path, by), &mut result)?,
        Input::Html(path) => {
            reading_result(reading::import_html(space, path, None, by), &mut result)?
        }
        Input::Url(url) => reading_result(reading::import_url(space, url, by), &mut result)?,
        Input::KindleClippings(path) => reading_result(
            reading::import_kindle_clippings(space, path, by),
            &mut result,
        )?,
        Input::KindleNotebook(path) => reading_result(
            reading::import_kindle_notebook(space, path, by),
            &mut result,
        )?,
        Input::ReadwiseCsv(path) => {
            reading_result(reading::import_readwise_csv(space, path, by), &mut result)?
        }
        Input::ReadwiseMarkdown(path) => reading_result(
            reading::import_readwise_markdown(space, path, by),
            &mut result,
        )?,
        Input::App(app, path) => {
            let report = apps::import_notes(space, *app, path, by).map_err(app_error)?;
            for n in &report.created {
                result.node(Some(n.node_id), n.title.clone(), ImportOutcome::Created);
            }
            for n in &report.updated {
                result.node(Some(n.node_id), n.title.clone(), ImportOutcome::Updated);
            }
            result.unchanged += report.unchanged;
            result.attached = report.attached;
            for s in &report.skipped {
                result.skip(s.path.clone(), s.reason.clone());
            }
            result.notes.extend(report.limits.iter().cloned());
        }
    }
    Ok(result)
}

fn reading_result(
    report: knode_vault::Result<ReadingReport>,
    result: &mut ImportResult,
) -> Result<(), ImportError> {
    let report = report.map_err(vault_error)?;
    for item in &report.imported {
        let outcome = match item.outcome {
            Outcome::Created => ImportOutcome::Created,
            Outcome::Updated => ImportOutcome::Updated,
            Outcome::Unchanged => ImportOutcome::Unchanged,
            _ => ImportOutcome::Edited,
        };
        result.node(Some(item.id), item.title.clone(), outcome);
        let node = result.nodes.last_mut().expect("just added");
        node.highlights = item.highlights;
        node.note = item.note.clone();
        result.highlights += item.highlights;
    }
    for s in &report.skipped {
        result.skip(s.path.clone(), s.reason.clone());
    }
    Ok(())
}

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn ingest_error(error: knode_ingest::ImportError) -> ImportError {
    match error {
        knode_ingest::ImportError::Space(error) => ImportError::Space(error),
        knode_ingest::ImportError::UnknownFormat(_) => ImportError::Unknown(error.to_string()),
        other => ImportError::Unreadable(other.to_string()),
    }
}

fn vault_error(error: knode_vault::Error) -> ImportError {
    match error {
        knode_vault::Error::Space(error) => ImportError::Space(error),
        other => ImportError::Unreadable(other.to_string()),
    }
}

fn app_error(error: apps::ImportError) -> ImportError {
    match error {
        apps::ImportError::Space(error) => ImportError::Space(error),
        apps::ImportError::NotFound(path) => ImportError::NotFound(path),
        e @ apps::ImportError::NoAccess { .. } => ImportError::NoAccess(e.to_string()),
        e @ (apps::ImportError::NotRecognized { .. } | apps::ImportError::UnknownApp(_)) => {
            ImportError::Unknown(e.to_string())
        }
        other => ImportError::Unreadable(other.to_string()),
    }
}

// One Markdown file ------------------------------------------------------------------------

fn markdown_file(
    space: &mut Space,
    file: &Path,
    by: &Provenance,
    result: &mut ImportResult,
) -> Result<(), ImportError> {
    let text = fs::read_to_string(file)
        .map_err(|e| ImportError::Unreadable(format!("{}: {e}", file.display())))?;
    let title = markdown_title(&text, file);
    // Importing the same file again does not make a copy.
    for found in space.find_by_title(&title)? {
        if let Some(node) = space.get_node(&found.id)?
            && node.body == text
        {
            result.node(Some(node.id), title, ImportOutcome::Unchanged);
            return Ok(());
        }
    }
    let source = by.clone().with_source(file.display().to_string());
    let node = space.create_node(NewNode::note(title.clone(), text), &source)?;
    result.node(Some(node.id), title, ImportOutcome::Created);
    Ok(())
}

/// The title of a Markdown note: its first heading, else the file name without extension.
pub fn markdown_title(text: &str, path: &Path) -> String {
    first_heading(text).unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Untitled".to_string())
    })
}

/// The text of the first ATX heading (`# Title` to `###### Title`) outside code fences and
/// front matter.
fn first_heading(text: &str) -> Option<String> {
    let mut fence: Option<&str> = None;
    let mut lines = text.lines().peekable();
    if lines.peek().is_some_and(|l| l.trim_end() == "---") {
        lines.next();
        for line in lines.by_ref() {
            if matches!(line.trim_end(), "---" | "...") {
                break;
            }
        }
    }
    for line in lines {
        let trimmed = line.trim_start();
        if line.len() - trimmed.len() > 3 {
            continue;
        }
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") {
            fence = Some("```");
            continue;
        }
        if trimmed.starts_with("~~~") {
            fence = Some("~~~");
            continue;
        }
        let level = trimmed.chars().take_while(|c| *c == '#').count();
        if !(1..=6).contains(&level) {
            continue;
        }
        let rest = &trimmed[level..];
        if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
            continue;
        }
        let title = rest.trim().trim_end_matches('#').trim();
        if !title.is_empty() {
            return Some(title.to_string());
        }
    }
    None
}

// One agent session ------------------------------------------------------------------------

/// Which agent wrote a session file, from its first records and its name.
fn detect_agent(path: &Path) -> Result<Agent, ImportError> {
    let file = fs::File::open(path)
        .map_err(|e| ImportError::Unreadable(format!("cannot read {}: {e}", path.display())))?;
    for line in BufReader::new(file).lines().take(50) {
        let Ok(line) = line else { break };
        let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if record.get("sessionId").is_some() {
            return Ok(Agent::ClaudeCode);
        }
        if record.get("type").and_then(|t| t.as_str()) == Some("session_meta")
            || (record.get("id").is_some() && record.get("instructions").is_some())
        {
            return Ok(Agent::Codex);
        }
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    if name.starts_with("rollout-") {
        return Ok(Agent::Codex);
    }
    Err(ImportError::Unknown(format!(
        "{} is not a Claude Code or Codex session file",
        path.display()
    )))
}

/// The session id in a file name: the whole name for Claude Code, the UUID at the end for
/// Codex (`rollout-<time>-<uuid>`).
fn session_id(agent: Agent, path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match agent {
        Agent::Codex => {
            let tail = stem
                .len()
                .checked_sub(36)
                .and_then(|start| stem.get(start..));
            match tail {
                Some(id) if id.chars().filter(|c| *c == '-').count() == 4 => id.to_string(),
                _ => stem,
            }
        }
        _ => stem,
    }
}

/// The Markdown of a conversation without its first line, the `# title` heading, since the
/// title is the node's own.
fn conversation_body(markdown: &str) -> String {
    match markdown.split_once('\n') {
        Some((first, rest)) if first.starts_with("# ") => rest.trim_start_matches('\n').to_string(),
        _ => markdown.to_string(),
    }
}

/// Imports a session as a `conversation` written by its agent, or updates the node of the same
/// session.
fn session(space: &mut Space, path: &Path, result: &mut ImportResult) -> Result<(), ImportError> {
    let agent = detect_agent(path)?;
    result.format = format!("{} session", agent.name());
    let mut reader = SessionReader::open(agent, path, session_id(agent, path));
    reader.poll().map_err(|e| {
        ImportError::Unreadable(format!("cannot read the session {}: {e}", path.display()))
    })?;
    let conversation = reader.into_conversation();
    if conversation.is_empty() {
        return Err(ImportError::Unknown(format!(
            "{} has no messages to import",
            path.display()
        )));
    }
    let title = conversation.title();
    let body = conversation_body(&to_markdown(&conversation));
    let mut by = Provenance::agent(agent.id()).with_session(conversation.session_id.clone());
    if let Some(project) = &conversation.project {
        by = by.with_source(project.display().to_string());
    }

    // The same session imported again updates its node instead of making a copy.
    for found in space.find_by_title(&title)? {
        let Some(node) = space.get_node(&found.id)? else {
            continue;
        };
        if node.kind == kinds::CONVERSATION
            && node.created_by.session.as_deref() == Some(conversation.session_id.as_str())
        {
            let update = NodeUpdate {
                body: Some(body),
                ..Default::default()
            };
            let before = node.updated_at;
            let node = space.update_node(&node.id, update, &by)?;
            let outcome = if node.updated_at == before {
                ImportOutcome::Unchanged
            } else {
                ImportOutcome::Updated
            };
            result.node(Some(node.id), title, outcome);
            return Ok(());
        }
    }
    let node = space.create_node(
        NewNode::note(title.clone(), body).with_kind(kinds::CONVERSATION),
        &by,
    )?;
    result.node(Some(node.id), title, ImportOutcome::Created);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_is_the_first_heading() {
        let p = Path::new("/x/file name.md");
        assert_eq!(markdown_title("# Hello #\n\ntext", p), "Hello");
        assert_eq!(
            markdown_title("intro\n\n## Second level\n", p),
            "Second level"
        );
        assert_eq!(markdown_title("```\n# not this\n```\n# This\n", p), "This");
        assert_eq!(markdown_title("---\ntitle: x\n---\n# After\n", p), "After");
        assert_eq!(markdown_title("#hashtag\n", p), "file name");
        assert_eq!(markdown_title("    # code\n", p), "file name");
        assert_eq!(markdown_title("", p), "file name");
    }

    #[test]
    fn session_ids_come_from_file_names() {
        assert_eq!(
            session_id(
                Agent::Codex,
                Path::new("rollout-2026-01-02T11-00-00-0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee.jsonl")
            ),
            "0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee"
        );
        assert_eq!(
            session_id(Agent::ClaudeCode, Path::new("/p/abc.jsonl")),
            "abc"
        );
    }

    #[test]
    fn conversation_body_drops_the_title() {
        assert_eq!(conversation_body("# T\n\n## User\nhi\n"), "## User\nhi\n");
        assert_eq!(conversation_body("no title"), "no title");
    }

    #[test]
    fn sources_by_name() {
        for source in ImportSource::ALL {
            assert_eq!(ImportSource::from_id(source.id()), Some(source));
        }
        assert_eq!(
            ImportSource::from_id("Apple_Notes"),
            Some(ImportSource::AppleNotes)
        );
        assert_eq!(ImportSource::from_id("onenote"), None);
    }

    #[test]
    fn urls_are_http_or_https_only() {
        assert!(is_url("https://example.com/a"));
        assert!(is_url("HTTP://example.com"));
        assert!(!is_url("file:///etc/hosts"));
        assert!(!is_url("example.com"));
        let refused = prepare(None, Some("ftp://example.com/x"), None).unwrap_err();
        assert!(refused.to_string().contains("only http and https"));
        let refused = prepare(None, Some("https://example.com"), Some(ImportSource::Pdf));
        assert!(refused.is_err());
    }
}
