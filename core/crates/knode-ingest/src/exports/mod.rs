//! Importing the data exports of web agents into a space: [`import_export`].
//!
//! | Export | Agent id | Status |
//! | --- | --- | --- |
//! | claude.ai data export (`conversations.json`, `projects.json`) | `claude-ai` | Fields as seen in exports, not documented by Anthropic |
//! | ChatGPT data export (`conversations.json`) | `chatgpt` | Fields as seen in exports, not documented by OpenAI |
//! | Google Takeout, My Activity, Gemini Apps (`MyActivity.json` or `.html`) | `gemini-web` | Not verified against a real export; English prompts and dates only |
//! | Perplexity | - | Not available: Perplexity offers no export of all threads, only one thread at a time as a PDF, Markdown or Word file |
//!
//! Each module documents the structure it reads. Every export is read as downloaded (the
//! `.zip`) or extracted (a folder), and large JSON files are streamed one conversation at a
//! time.

mod chatgpt;
mod claude_ai;
mod gemini;
mod html;
mod source;

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use knode_format::{
    ChangeOp, NewNode, NodeId, NodeUpdate, Properties, Provenance, Space, link_kinds,
};
use serde_json::Value;

use crate::capture::{Upserted, find_conversation, node_title, upsert_conversation};
use crate::model::{Agent, Conversation, Part};
use source::{Source, StreamError, each_element, file_name, parent};

/// The property that finds again the node of a Claude project: the project's uuid.
pub const CLAUDE_PROJECT_PROPERTY: &str = "claude-ai.project";

/// How many bytes of `conversations.json` are read to tell a Claude export from a ChatGPT one
/// when the files beside it do not.
const PEEK_BYTES: u64 = 64 * 1024;

/// The kind of a data export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ExportFormat {
    /// The claude.ai data export.
    ClaudeAi,
    /// The ChatGPT data export.
    ChatGpt,
    /// Gemini Apps activity from Google Takeout.
    GeminiTakeout,
}

impl ExportFormat {
    /// The agent whose conversations the export holds.
    pub fn agent(self) -> Agent {
        match self {
            ExportFormat::ClaudeAi => Agent::ClaudeAi,
            ExportFormat::ChatGpt => Agent::ChatGpt,
            ExportFormat::GeminiTakeout => Agent::GeminiWeb,
        }
    }
}

impl fmt::Display for ExportFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ExportFormat::ClaudeAi => "Claude export",
            ExportFormat::ChatGpt => "ChatGPT export",
            ExportFormat::GeminiTakeout => "Google Takeout (Gemini Apps)",
        })
    }
}

/// Why an import failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum ImportError {
    /// The export could not be read.
    Io(io::Error),
    /// A file of the export is not the JSON it should be.
    Json {
        /// The file, inside the export.
        file: String,
        /// What was wrong.
        error: serde_json::Error,
    },
    /// The path is not an export Knode can read.
    UnknownFormat(PathBuf),
    /// The space could not be written.
    Space(knode_format::Error),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::Io(error) => write!(f, "the export could not be read: {error}"),
            ImportError::Json { file, error } => write!(f, "{file} could not be read: {error}"),
            ImportError::UnknownFormat(path) => write!(
                f,
                "{} is not a Claude, ChatGPT or Google Takeout (Gemini Apps) export",
                path.display()
            ),
            ImportError::Space(error) => write!(f, "the import could not write the space: {error}"),
        }
    }
}

impl std::error::Error for ImportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ImportError::Io(error) => Some(error),
            ImportError::Json { error, .. } => Some(error),
            ImportError::UnknownFormat(_) => None,
            ImportError::Space(error) => Some(error),
        }
    }
}

impl From<io::Error> for ImportError {
    fn from(error: io::Error) -> Self {
        ImportError::Io(error)
    }
}

impl From<knode_format::Error> for ImportError {
    fn from(error: knode_format::Error) -> Self {
        ImportError::Space(error)
    }
}

/// One node an import created or changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    /// The node.
    pub node_id: NodeId,
    /// [`ChangeOp::Create`] for a new node, [`ChangeOp::Update`] for one that changed.
    pub op: ChangeOp,
    /// The id the export gave it: the conversation id, or the project's uuid.
    pub export_id: String,
    /// Its title.
    pub title: String,
}

/// Something in an export that was not imported, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    /// What it was: an id, or the start of a prompt.
    pub item: String,
    /// Why it was skipped.
    pub reason: String,
}

/// What [`import_export`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    /// The kind of export read.
    pub format: ExportFormat,
    /// The `conversation` nodes created or changed, in the order they were written.
    pub conversations: Vec<Imported>,
    /// The project nodes created or changed (Claude projects).
    pub projects: Vec<Imported>,
    /// Conversations and projects whose node was already up to date.
    pub unchanged: usize,
    /// Conversations with no messages; no node is written for them.
    pub empty: usize,
    /// What could not be imported; the rest was.
    pub skipped: Vec<Skipped>,
}

impl ImportReport {
    fn new(format: ExportFormat) -> Self {
        Self {
            format,
            conversations: Vec::new(),
            projects: Vec::new(),
            unchanged: 0,
            empty: 0,
            skipped: Vec::new(),
        }
    }

    /// The conversation nodes created.
    pub fn created(&self) -> impl Iterator<Item = &Imported> {
        self.conversations
            .iter()
            .filter(|c| c.op == ChangeOp::Create)
    }

    /// The conversation nodes updated.
    pub fn updated(&self) -> impl Iterator<Item = &Imported> {
        self.conversations
            .iter()
            .filter(|c| c.op == ChangeOp::Update)
    }
}

/// The kind of export at `path`, a `.zip` as downloaded or the folder it was extracted to.
pub fn detect_export(path: impl AsRef<Path>) -> Result<ExportFormat, ImportError> {
    let path = path.as_ref();
    let mut source = Source::open(path)?;
    Ok(locate(&mut source, path)?.format())
}

/// Imports the conversations of a data export into `space`, written by `by`: one
/// `conversation` node per conversation, like [`Capture`](crate::Capture) writes, with the
/// title of the export, the transcript as its body and the `conversation.*` properties (agent
/// `claude-ai`, `chatgpt` or `gemini-web`, the export's conversation id as the session, the
/// model when the export names it, and the times).
///
/// `path` is the `.zip` as downloaded or the folder it was extracted to; see the [module
/// documentation](self) for the exports read. Each write has the provenance `by` with the
/// conversation id as its session and `path` as its source.
///
/// A conversation is found again by its agent and session, so importing an export again, or a
/// newer export of the same account, updates the nodes instead of duplicating them; a
/// conversation deleted from the space since is written again. Conversations with no messages
/// are not written. Claude projects become `note` nodes found again by
/// [`CLAUDE_PROJECT_PROPERTY`], and their conversations, when the export says which, link to
/// them as `part-of`.
///
/// ```no_run
/// use knode_format::{Provenance, Space};
/// use knode_ingest::import_export;
///
/// let mut space = Space::open("/Users/example/Knode/Personal.knode")?;
/// let report = import_export(&mut space, "/Users/example/Downloads/export.zip",
///     &Provenance::person("example"))?;
/// println!("{}: {} new conversations", report.format, report.created().count());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn import_export(
    space: &mut Space,
    path: impl AsRef<Path>,
    by: &Provenance,
) -> Result<ImportReport, ImportError> {
    let path = path.as_ref();
    let mut source = Source::open(path)?;
    let located = locate(&mut source, path)?;
    let mut writer = Writer {
        space,
        by,
        source_path: path.to_string_lossy().into_owned(),
        report: ImportReport::new(located.format()),
    };
    match located {
        Located::Claude {
            conversations,
            projects,
        } => {
            let mut nodes = BTreeMap::new();
            if let Some(file) = projects {
                stream(&mut source, &file, |value| {
                    match claude_ai::project(&value) {
                        Some(project) => {
                            let id = writer.write_project(&project)?;
                            nodes.insert(project.uuid, id);
                        }
                        None => writer.skip(&value, "a project with no uuid"),
                    }
                    Ok(())
                })?;
            }
            for file in conversations {
                stream(&mut source, &file, |value| {
                    match claude_ai::conversation(&value) {
                        Some((conversation, project)) => {
                            let node = writer.write_conversation(&conversation)?;
                            let project = project.and_then(|uuid| nodes.get(&uuid));
                            if let (Some(node), Some(project)) = (node, project) {
                                writer.space.link(
                                    &node,
                                    project,
                                    link_kinds::PART_OF,
                                    writer.by,
                                )?;
                            }
                        }
                        None => writer.skip(&value, "a conversation with no uuid"),
                    }
                    Ok(())
                })?;
            }
        }
        Located::ChatGpt(files) => {
            for file in files {
                stream(&mut source, &file, |value| {
                    match chatgpt::conversation(&value) {
                        Some(conversation) => {
                            writer.write_conversation(&conversation)?;
                        }
                        None => writer.skip(&value, "a conversation with no id"),
                    }
                    Ok(())
                })?;
            }
        }
        Located::Gemini { file, json } => {
            let mut activities = Vec::new();
            if json {
                stream(&mut source, &file, |value| {
                    match gemini::json_activity(&value) {
                        Ok(Some(activity)) => activities.push(activity),
                        Ok(None) => {}
                        Err(reason) => writer.report.skipped.push(Skipped {
                            item: file.clone(),
                            reason,
                        }),
                    }
                    Ok(())
                })?;
            } else {
                let page = source.read_to_string(&file)?;
                let (read, skipped) = gemini::html_activities(&page);
                activities = read;
                writer.report.skipped.extend(skipped);
            }
            for conversation in gemini::conversations(activities) {
                writer.write_conversation(&conversation)?;
            }
        }
    }
    Ok(writer.report)
}

/// Where the conversations of an export are.
enum Located {
    Claude {
        conversations: Vec<String>,
        projects: Option<String>,
    },
    ChatGpt(Vec<String>),
    Gemini {
        file: String,
        json: bool,
    },
}

impl Located {
    fn format(&self) -> ExportFormat {
        match self {
            Located::Claude { .. } => ExportFormat::ClaudeAi,
            Located::ChatGpt(_) => ExportFormat::ChatGpt,
            Located::Gemini { .. } => ExportFormat::GeminiTakeout,
        }
    }
}

/// Finds the files of the export: Gemini activity in a Takeout, else the shallowest
/// `conversations.json`, told apart by the files beside it or, failing that, by its first
/// bytes.
fn locate(source: &mut Source, path: &Path) -> Result<Located, ImportError> {
    let names = source.names()?;
    let gemini_dir = |name: &str| {
        let dir = file_name(parent(name));
        dir.contains("Gemini") || dir.contains("Bard")
    };
    let activity = |wanted: &str| {
        names
            .iter()
            .find(|name| file_name(name) == wanted && gemini_dir(name))
            .cloned()
    };
    if let Some(file) = activity("MyActivity.json") {
        return Ok(Located::Gemini { file, json: true });
    }
    if let Some(file) = activity("MyActivity.html") {
        return Ok(Located::Gemini { file, json: false });
    }

    let Some(main) = names
        .iter()
        .filter(|name| file_name(name) == "conversations.json")
        .min_by_key(|name| (name.matches('/').count(), name.len()))
        .cloned()
    else {
        return Err(ImportError::UnknownFormat(path.to_path_buf()));
    };
    let dir = parent(&main).to_string();
    let beside = |file: &str| {
        let wanted = if dir.is_empty() {
            file.to_string()
        } else {
            format!("{dir}/{file}")
        };
        names.contains(&wanted).then_some(wanted)
    };
    // Exports split into numbered files (`conversations-000.json`) are read too.
    let mut files = vec![main.clone()];
    files.extend(
        names
            .iter()
            .filter(|name| parent(name) == dir && is_numbered_part(file_name(name)))
            .cloned(),
    );
    let claude = if beside("chat.html").is_some() || beside("message_feedback.json").is_some() {
        false
    } else if beside("projects.json").is_some() || beside("users.json").is_some() {
        true
    } else {
        let head = source.peek(&main, PEEK_BYTES)?;
        let head = String::from_utf8_lossy(&head);
        match (head.find("\"chat_messages\""), head.find("\"mapping\"")) {
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (Some(claude), Some(chatgpt)) => claude < chatgpt,
            (None, None) if head.trim_start().starts_with('[') => {
                // An empty array or an unknown shape: nothing to tell it by.
                return Err(ImportError::UnknownFormat(path.to_path_buf()));
            }
            (None, None) => return Err(ImportError::UnknownFormat(path.to_path_buf())),
        }
    };
    Ok(if claude {
        Located::Claude {
            conversations: files,
            projects: beside("projects.json"),
        }
    } else {
        Located::ChatGpt(files)
    })
}

/// Whether a file name is `conversations-<digits>.json`.
fn is_numbered_part(name: &str) -> bool {
    name.strip_prefix("conversations-")
        .and_then(|rest| rest.strip_suffix(".json"))
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// Streams the array in `file` to `each`.
fn stream<F>(source: &mut Source, file: &str, each: F) -> Result<(), ImportError>
where
    F: FnMut(Value) -> Result<(), ImportError>,
{
    let reader = source.open_file(file)?;
    each_element(reader, each).map_err(|error| match error {
        StreamError::Callback(error) => error,
        StreamError::Json(error) if error.is_io() => ImportError::Io(error.into()),
        StreamError::Json(error) => ImportError::Json {
            file: file.to_string(),
            error,
        },
    })
}

struct Writer<'a> {
    space: &'a mut Space,
    by: &'a Provenance,
    source_path: String,
    report: ImportReport,
}

impl Writer<'_> {
    fn provenance(&self, session: &str) -> Provenance {
        self.by
            .clone()
            .with_session(session)
            .with_source(&self.source_path)
    }

    fn skip(&mut self, value: &Value, reason: &str) {
        let item = crate::text::first_str(value, &["title", "name"])
            .map(|title| crate::text::one_line(title, 60))
            .unwrap_or_default();
        self.report.skipped.push(Skipped {
            item,
            reason: reason.to_string(),
        });
    }

    /// Writes a conversation, and returns its node unless it is empty.
    fn write_conversation(
        &mut self,
        conversation: &Conversation,
    ) -> Result<Option<NodeId>, ImportError> {
        if conversation.is_empty() {
            self.report.empty += 1;
            return Ok(None);
        }
        let existing = find_conversation(self.space, conversation.agent, &conversation.session_id)?;
        let by = self.provenance(&conversation.session_id);
        let imported = |node_id, op| Imported {
            node_id,
            op,
            export_id: conversation.session_id.clone(),
            title: node_title(conversation),
        };
        let id = match upsert_conversation(self.space, conversation, existing, &by, false)? {
            Upserted::Created(id) => {
                self.report
                    .conversations
                    .push(imported(id, ChangeOp::Create));
                id
            }
            Upserted::Updated(id) => {
                self.report
                    .conversations
                    .push(imported(id, ChangeOp::Update));
                id
            }
            Upserted::Unchanged(id) => {
                self.report.unchanged += 1;
                id
            }
        };
        Ok(Some(id))
    }

    /// Writes a Claude project as a note, and returns its node.
    fn write_project(&mut self, project: &claude_ai::Project) -> Result<NodeId, ImportError> {
        let by = self.provenance(&project.uuid);
        let existing = self
            .space
            .find_by_property(CLAUDE_PROJECT_PROPERTY, &project.uuid)?
            .into_iter()
            .next();
        let imported = |node_id, op| Imported {
            node_id,
            op,
            export_id: project.uuid.clone(),
            title: project.title.clone(),
        };
        let Some(node) = existing else {
            let mut properties = Properties::new();
            properties.insert(CLAUDE_PROJECT_PROPERTY.to_string(), project.uuid.clone());
            let node = self.space.create_node_with_properties(
                NewNode::note(project.title.clone(), project.body.clone()),
                &properties,
                &by,
            )?;
            self.report
                .projects
                .push(imported(node.id, ChangeOp::Create));
            return Ok(node.id);
        };
        let current = self
            .space
            .get_node(&node.id)?
            .ok_or(knode_format::Error::NotFound(node.id))?;
        if current.title != project.title || current.body != project.body {
            self.space.update_node(
                &node.id,
                NodeUpdate {
                    title: Some(project.title.clone()),
                    body: Some(project.body.clone()),
                    ..Default::default()
                },
                &by,
            )?;
            self.report
                .projects
                .push(imported(node.id, ChangeOp::Update));
        } else {
            self.report.unchanged += 1;
        }
        Ok(node.id)
    }
}

/// Extensions of attached files shown as an image placeholder.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "heic", "bmp", "svg"];

/// An image placeholder for an image file, else a line naming the file.
pub(crate) fn file_part(name: &str) -> Part {
    let extension = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase());
    if extension.is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.as_str())) {
        Part::Image
    } else {
        Part::Text {
            text: format!("_[attached: {}]_", name.replace(['[', ']'], "")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_numbered_parts() {
        assert!(is_numbered_part("conversations-000.json"));
        assert!(!is_numbered_part("conversations-.json"));
        assert!(!is_numbered_part("conversations-a.json"));
        assert!(!is_numbered_part("conversations.json"));
    }

    #[test]
    fn names_files_and_images() {
        assert_eq!(file_part("Photo.JPG"), Part::Image);
        assert_eq!(
            file_part("notes [v2].txt"),
            Part::Text {
                text: "_[attached: notes v2.txt]_".into()
            }
        );
    }
}
