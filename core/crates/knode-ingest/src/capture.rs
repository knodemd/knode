//! Writing conversations into a space and keeping them current.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use knode_format::{
    ChangeOp, NewNode, NodeId, NodeUpdate, Properties, Provenance, Space, kinds, property_keys,
};
use serde::{Deserialize, Serialize};

use crate::discover::{SessionFile, default_root, discover, session_file};
use crate::filter::ProjectFilter;
use crate::model::{Agent, Conversation};
use crate::reader::{self, SessionReader};
use crate::render::to_markdown;

/// The version of the state file this library writes.
const STATE_VERSION: u32 = 1;

/// Why capture failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum CaptureError {
    /// The space could not be read or written.
    Space(knode_format::Error),
    /// The state file could not be read or written.
    State(io::Error),
    /// The session folders could not be listed.
    Discover(io::Error),
    /// The file system watcher could not start.
    Watch(notify::Error),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::Space(error) => write!(f, "capture could not write the space: {error}"),
            CaptureError::State(error) => write!(f, "capture state could not be saved: {error}"),
            CaptureError::Discover(error) => {
                write!(f, "the session folders could not be listed: {error}")
            }
            CaptureError::Watch(error) => {
                write!(f, "the session folders cannot be watched: {error}")
            }
        }
    }
}

impl std::error::Error for CaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CaptureError::Space(error) => Some(error),
            CaptureError::State(error) | CaptureError::Discover(error) => Some(error),
            CaptureError::Watch(error) => Some(error),
        }
    }
}

impl From<knode_format::Error> for CaptureError {
    fn from(error: knode_format::Error) -> Self {
        CaptureError::Space(error)
    }
}

impl From<notify::Error> for CaptureError {
    fn from(error: notify::Error) -> Self {
        CaptureError::Watch(error)
    }
}

/// One conversation node that a sync created or changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    /// The node.
    pub node_id: NodeId,
    /// [`ChangeOp::Create`] for a new conversation, [`ChangeOp::Update`] for one that grew.
    pub op: ChangeOp,
    /// The agent of the conversation.
    pub agent: Agent,
    /// The agent's id for the session.
    pub session_id: String,
    /// The session file it was read from.
    pub path: PathBuf,
}

/// What one sync did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureReport {
    /// The nodes created or changed, in the order they were written.
    pub changes: Vec<Captured>,
    /// Session files looked at.
    pub files: usize,
    /// Files with nothing new since the last sync, or whose node was already up to date.
    pub unchanged: usize,
    /// Files whose conversation has no messages yet; no node is written for them.
    pub empty: usize,
    /// Files left out by the project filter. They are not read again until they change or the
    /// filter does.
    pub filtered: usize,
    /// Files that were cut or replaced, and were read again from the start.
    pub restarted: usize,
    /// Conversations whose node was deleted in the space; they are not written again.
    pub deleted: usize,
    /// Files that could not be read, with the reason. They are tried again next time.
    pub failed: Vec<(PathBuf, String)>,
}

impl CaptureReport {
    /// The nodes created.
    pub fn created(&self) -> impl Iterator<Item = &Captured> {
        self.changes.iter().filter(|c| c.op == ChangeOp::Create)
    }

    /// The nodes updated.
    pub fn updated(&self) -> impl Iterator<Item = &Captured> {
        self.changes.iter().filter(|c| c.op == ChangeOp::Update)
    }
}

/// What capture remembers about one session file between syncs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileState {
    agent: Agent,
    /// The byte offset read so far: the end of the last complete line.
    offset: u64,
    /// The conversation read up to `offset`. `None` for a file left out by the filter, whose
    /// text is never kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    conversation: Option<Conversation>,
    /// The node written for the conversation, once there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    node: Option<NodeId>,
    /// Whether the project filter left the file out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    excluded: bool,
}

/// The state file: every session file read so far, for one space.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    version: u32,
    /// The id of the space the conversations were written to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    space: Option<String>,
    /// The projects the filter left out when the files were read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    excluded: Vec<PathBuf>,
    #[serde(default)]
    files: BTreeMap<PathBuf, FileState>,
}

/// Syncs agent session files into a space: one `conversation` node per session, kept current
/// as the session grows.
///
/// Each conversation is found again by its `conversation.agent` and `conversation.session`
/// [properties](knode_format::property_keys), so reading a session again updates its node
/// instead of creating another one. The body is the Markdown of [`to_markdown`] and the title
/// [`Conversation::title`]; the agent, session, project, branch, model and times are
/// properties. Every write has the provenance
/// `Provenance::agent(<agent id>).with_session(<session id>).with_source(<file path>)`.
/// Updates use [`Space::update_node_coalescing`], so a session followed live keeps one history
/// row for each run of updates instead of one for every batch; an edit by anyone else in
/// between stays in the history with the version it changed.
///
/// What was read from each file (its offset and conversation so far) is saved in a JSON state
/// file after every sync, so after a restart only new lines are read. A file that was cut or
/// replaced is read again from the start. Conversations with no messages are not written, and a
/// conversation whose node was deleted from the space is not written again.
///
/// ```no_run
/// use knode_format::Space;
/// use knode_ingest::{Capture, ProjectFilter};
///
/// let mut space = Space::open("/Users/example/Knode/Personal.knode")?;
/// let mut capture = Capture::open("/Users/example/state.json")?
///     .with_filter(ProjectFilter::excluding(["/Users/example/private"]));
/// let report = capture.sync_all(&mut space)?;
/// println!("{} new conversations", report.created().count());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct Capture {
    state_path: PathBuf,
    state: State,
    filter: ProjectFilter,
    roots: Vec<(Agent, PathBuf)>,
}

impl Capture {
    /// Capture with its state in `state_path`, read now if it exists, and the default session
    /// folders of every agent (see [`default_root`]).
    ///
    /// A state file that cannot be parsed, or that was written by a newer version, is ignored:
    /// every session is read again from the start, which finds and updates the same nodes.
    pub fn open(state_path: impl Into<PathBuf>) -> Result<Self, CaptureError> {
        let state_path = state_path.into();
        let state = match fs::read(&state_path) {
            Ok(bytes) => serde_json::from_slice::<State>(&bytes)
                .ok()
                .filter(|state| state.version == STATE_VERSION)
                .unwrap_or_default(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => State::default(),
            Err(error) => return Err(CaptureError::State(error)),
        };
        let roots = [Agent::ClaudeCode, Agent::Codex]
            .into_iter()
            .filter_map(|agent| default_root(agent).map(|root| (agent, root)))
            .collect();
        Ok(Self {
            state_path,
            state,
            filter: ProjectFilter::new(),
            roots,
        })
    }

    /// Uses these session folders instead of the default ones.
    pub fn with_roots<I, P>(mut self, roots: I) -> Self
    where
        I: IntoIterator<Item = (Agent, P)>,
        P: Into<PathBuf>,
    {
        self.roots = roots
            .into_iter()
            .map(|(agent, root)| (agent, root.into()))
            .collect();
        self
    }

    /// Uses this project filter.
    pub fn with_filter(mut self, filter: ProjectFilter) -> Self {
        self.set_filter(filter);
        self
    }

    /// Changes the project filter. Files the old filter left out are read again at the next
    /// sync; conversations already written stay in the space and are no longer updated.
    pub fn set_filter(&mut self, filter: ProjectFilter) {
        self.filter = filter;
    }

    /// The project filter.
    pub fn filter(&self) -> &ProjectFilter {
        &self.filter
    }

    /// The session folders, by agent.
    pub fn roots(&self) -> &[(Agent, PathBuf)] {
        &self.roots
    }

    /// The state file.
    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    /// Reads every session file in the session folders and writes what changed, then saves the
    /// state. Use it once at start, to catch up, and from time to time to find what the
    /// watcher missed. Files that are gone are forgotten; their nodes stay.
    pub fn sync_all(&mut self, space: &mut Space) -> Result<CaptureReport, CaptureError> {
        let mut files = Vec::new();
        for (agent, root) in &self.roots {
            files.extend(discover(*agent, root).map_err(CaptureError::Discover)?);
        }
        let present: std::collections::BTreeSet<&Path> =
            files.iter().map(|f| f.path.as_path()).collect();
        let roots = &self.roots;
        self.state.files.retain(|path, _| {
            present.contains(path.as_path())
                || !roots.iter().any(|(_, root)| path.starts_with(root))
        });
        self.sync_files(space, &files)
    }

    /// Reads the given paths, such as the ones a [`Watcher`](crate::Watcher) reported, and
    /// writes what changed, then saves the state. Paths that are not session files under one of
    /// the session folders are ignored; removed files are forgotten.
    pub fn sync_paths<P: AsRef<Path>>(
        &mut self,
        space: &mut Space,
        paths: &[P],
    ) -> Result<CaptureReport, CaptureError> {
        let mut files = Vec::new();
        for path in paths {
            let Some((agent, root, path)) = self.locate(path.as_ref()) else {
                continue;
            };
            match session_file(agent, &root, &path) {
                Some(file) => files.push(file),
                None => {
                    if !path.exists() {
                        self.state.files.remove(&path);
                    }
                }
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files.dedup_by(|a, b| a.path == b.path);
        self.sync_files(space, &files)
    }

    /// Writes the state file now. Syncs save it themselves.
    pub fn save(&self) -> Result<(), CaptureError> {
        let bytes = serde_json::to_vec(&self.state)
            .map_err(|error| CaptureError::State(io::Error::other(error)))?;
        if let Some(parent) = self
            .state_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(CaptureError::State)?;
        }
        let mut temporary = self.state_path.clone().into_os_string();
        temporary.push(".tmp");
        let temporary = PathBuf::from(temporary);
        fs::write(&temporary, bytes).map_err(CaptureError::State)?;
        fs::rename(&temporary, &self.state_path).map_err(CaptureError::State)
    }

    /// The agent and root a path belongs to, and the path written under that root. Paths
    /// reported under a root's canonical form (macOS reports `/private/var` for `/var`) are
    /// mapped back to the root as configured.
    fn locate(&self, path: &Path) -> Option<(Agent, PathBuf, PathBuf)> {
        for (agent, root) in &self.roots {
            if path.starts_with(root) {
                return Some((*agent, root.clone(), path.to_path_buf()));
            }
            if let Ok(canonical) = root.canonicalize() {
                if let Ok(rest) = path.strip_prefix(&canonical) {
                    return Some((*agent, root.clone(), root.join(rest)));
                }
            }
        }
        None
    }

    fn sync_files(
        &mut self,
        space: &mut Space,
        files: &[SessionFile],
    ) -> Result<CaptureReport, CaptureError> {
        let space_id = space.manifest().id.clone();
        if self.state.space.as_deref() != Some(space_id.as_str()) {
            // The state belongs to another space: read everything again for this one.
            self.state = State {
                version: STATE_VERSION,
                space: Some(space_id),
                excluded: Vec::new(),
                files: BTreeMap::new(),
            };
        }
        if self.state.excluded != self.filter.excluded() {
            // The filter changed: what it left out before is read again.
            self.state.files.retain(|_, file| !file.excluded);
            self.state.excluded = self.filter.excluded().to_vec();
        }
        let mut report = CaptureReport::default();
        let mut outcome = Ok(());
        for file in files {
            report.files += 1;
            if let Err(error) = self.sync_file(space, file, &mut report) {
                outcome = Err(error);
                break;
            }
        }
        // Save what was done even when a write failed, so it is not done twice.
        self.save()?;
        outcome.map(|()| report)
    }

    fn sync_file(
        &mut self,
        space: &mut Space,
        file: &SessionFile,
        report: &mut CaptureReport,
    ) -> Result<(), CaptureError> {
        if !self.filter.may_allow_file(file) {
            self.state.files.insert(
                file.path.clone(),
                FileState {
                    agent: file.agent,
                    offset: file.len,
                    conversation: None,
                    node: None,
                    excluded: true,
                },
            );
            report.filtered += 1;
            return Ok(());
        }
        let previous = self
            .state
            .files
            .get(&file.path)
            .filter(|state| state.agent == file.agent)
            .cloned();
        if let Some(state) = &previous {
            if state.excluded {
                // Its project does not change as the file grows: skip it without reading.
                self.state.files.insert(
                    file.path.clone(),
                    FileState {
                        offset: file.len,
                        ..state.clone()
                    },
                );
                report.filtered += 1;
                return Ok(());
            }
            if state.offset == file.len && state.node.is_some() {
                report.unchanged += 1;
                return Ok(());
            }
        }
        let node = previous.as_ref().and_then(|state| state.node);
        let mut reader = match previous {
            Some(FileState {
                offset,
                conversation: Some(conversation),
                ..
            }) => SessionReader::resume(&file.path, conversation, offset),
            _ => SessionReader::new(file),
        };
        let progress = match reader.poll() {
            Ok(progress) => progress,
            Err(reader::Error::Truncated { .. }) => {
                report.restarted += 1;
                reader = SessionReader::new(file);
                match reader.poll() {
                    Ok(progress) => progress,
                    Err(error) => return self.read_failed(file, error, report),
                }
            }
            Err(error) => return self.read_failed(file, error, report),
        };
        let conversation = reader.conversation();
        let offset = reader.offset();

        if !self.filter.allows(conversation) {
            self.state.files.insert(
                file.path.clone(),
                FileState {
                    agent: file.agent,
                    offset,
                    conversation: None,
                    node: None,
                    excluded: true,
                },
            );
            report.filtered += 1;
            return Ok(());
        }
        let mut state = FileState {
            agent: file.agent,
            offset,
            conversation: Some(conversation.clone()),
            node,
            excluded: false,
        };
        if conversation.is_empty() {
            report.empty += 1;
        } else if progress.records == 0 && node.is_some() {
            report.unchanged += 1;
        } else {
            match write_conversation(space, conversation, &file.path, node)? {
                Written::Changed(captured) => {
                    state.node = Some(captured.node_id);
                    report.changes.push(captured);
                }
                Written::Unchanged(id) => {
                    state.node = Some(id);
                    report.unchanged += 1;
                }
                Written::Deleted => report.deleted += 1,
            }
        }
        self.state.files.insert(file.path.clone(), state);
        Ok(())
    }

    fn read_failed(
        &mut self,
        file: &SessionFile,
        error: reader::Error,
        report: &mut CaptureReport,
    ) -> Result<(), CaptureError> {
        match error {
            reader::Error::Io(io) if io.kind() == io::ErrorKind::NotFound => {
                self.state.files.remove(&file.path);
            }
            error => report.failed.push((file.path.clone(), error.to_string())),
        }
        Ok(())
    }
}

enum Written {
    Changed(Captured),
    Unchanged(NodeId),
    Deleted,
}

/// The properties of a conversation node.
pub(crate) fn conversation_properties(conversation: &Conversation) -> Properties {
    let mut properties = Properties::new();
    let mut set = |key: &str, value: Option<String>| {
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            properties.insert(key.to_string(), value);
        }
    };
    let time = |at: Option<chrono::DateTime<chrono::Utc>>| {
        at.map(|at| knode_format::Timestamp(at.timestamp_millis()).to_string())
    };
    set(
        property_keys::CONVERSATION_AGENT,
        Some(conversation.agent.id().to_string()),
    );
    set(
        property_keys::CONVERSATION_SESSION,
        Some(conversation.session_id.clone()),
    );
    set(
        property_keys::CONVERSATION_PROJECT,
        conversation
            .project
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
    );
    set(
        property_keys::CONVERSATION_BRANCH,
        conversation.git_branch.clone(),
    );
    set(
        property_keys::CONVERSATION_MODEL,
        conversation.model.clone(),
    );
    set(
        property_keys::CONVERSATION_STARTED_AT,
        time(conversation.started_at),
    );
    set(
        property_keys::CONVERSATION_ENDED_AT,
        time(conversation.ended_at),
    );
    properties
}

/// The title of a conversation node: [`Conversation::title`] without the characters a title
/// cannot have (`[`, `]` and `|`, which `[[links]]` use), whitespace collapsed.
pub(crate) fn node_title(conversation: &Conversation) -> String {
    let title = conversation
        .title()
        .replace('[', "(")
        .replace(']', ")")
        .replace('|', "/");
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        format!("{} session", conversation.agent)
    } else {
        title
    }
}

/// The node of a conversation: the oldest with its agent and session properties.
fn find_conversation(
    space: &Space,
    agent: Agent,
    session_id: &str,
) -> Result<Option<NodeId>, CaptureError> {
    for node in space.find_by_property(property_keys::CONVERSATION_SESSION, session_id)? {
        if space
            .property(&node.id, property_keys::CONVERSATION_AGENT)?
            .as_deref()
            == Some(agent.id())
        {
            return Ok(Some(node.id));
        }
    }
    Ok(None)
}

fn write_conversation(
    space: &mut Space,
    conversation: &Conversation,
    path: &Path,
    known: Option<NodeId>,
) -> Result<Written, CaptureError> {
    let agent = conversation.agent;
    let session_id = conversation.session_id.clone();
    let by = Provenance::agent(agent.id())
        .with_session(&session_id)
        .with_source(path.to_string_lossy());
    let title = node_title(conversation);
    let body = to_markdown(conversation);
    let properties = conversation_properties(conversation);
    let captured = |node_id, op| Captured {
        node_id,
        op,
        agent,
        session_id: session_id.clone(),
        path: path.to_path_buf(),
    };

    let Some(id) = find_conversation(space, agent, &session_id)? else {
        if known.is_some() {
            // Written before and deleted since: the person removed it on purpose.
            return Ok(Written::Deleted);
        }
        let node = space.create_node_with_properties(
            NewNode::note(title, body).with_kind(kinds::CONVERSATION),
            &properties,
            &by,
        )?;
        return Ok(Written::Changed(captured(node.id, ChangeOp::Create)));
    };
    let current = space
        .get_node(&id)?
        .ok_or(knode_format::Error::NotFound(id))?;
    let text_changed = current.title != title || current.body != body;
    if text_changed {
        // One history row per run of updates from the session: the transcript only grows, so
        // the newest version holds every earlier one.
        space.update_node_coalescing(
            &id,
            NodeUpdate {
                title: Some(title),
                body: Some(body),
                ..Default::default()
            },
            &by,
        )?;
    }
    let properties_changed = space.set_properties(&id, &properties, &by)?;
    Ok(if text_changed || properties_changed {
        Written::Changed(captured(id, ChangeOp::Update))
    } else {
        Written::Unchanged(id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Part, Role, Title, TitleSource};

    #[test]
    fn titles_lose_link_characters() {
        let mut conversation = Conversation::new(Agent::Codex, "s");
        conversation.recorded_title = Some(Title {
            text: "Fix [[links]] | and   tables".into(),
            source: TitleSource::User,
        });
        assert_eq!(node_title(&conversation), "Fix ((links)) / and tables");
        conversation.recorded_title = Some(Title {
            text: "[ ]".into(),
            source: TitleSource::User,
        });
        assert_eq!(node_title(&conversation), "( )");
    }

    #[test]
    fn properties_leave_out_what_is_unknown() {
        let mut conversation = Conversation::new(Agent::ClaudeCode, "s-1");
        conversation.push_part(
            Role::User,
            None,
            Part::Text {
                text: "Hello".into(),
            },
        );
        let properties = conversation_properties(&conversation);
        assert_eq!(properties.len(), 2);
        assert_eq!(properties[property_keys::CONVERSATION_AGENT], "claude-code");
        assert_eq!(properties[property_keys::CONVERSATION_SESSION], "s-1");
    }
}
