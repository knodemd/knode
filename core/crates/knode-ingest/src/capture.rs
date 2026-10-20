//! Writing conversations into a space and keeping them current.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use knode_format::{
    ChangeOp, NewNode, NodeId, NodeUpdate, Properties, Provenance, Space, kinds, property_keys,
};
use serde::{Deserialize, Serialize};

use crate::discover::{SessionFile, changed_sessions, default_roots, discover};
use crate::filter::ProjectFilter;
use crate::model::{Agent, Conversation};
use crate::reader::{self, SessionReader};
use crate::render::to_markdown;

/// The version of the state file this library writes. Version 1, with one node per file, is
/// still read.
const STATE_VERSION: u32 = 2;

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
    /// The space it was written to: its key in [`Destinations`], the space's id for
    /// [`Capture::sync_all`] and [`Capture::sync_paths`].
    pub space: String,
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
    /// Files left out by the project filter, or whose conversation goes to no space. They are
    /// not read again until they change, or the filter or the routing does.
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
    /// The conversation read up to `offset`. `None` for a file left out by the filter, or whose
    /// conversation goes to no space: its text is never kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    conversation: Option<Conversation>,
    /// The node of the conversation in each space it was written to, by space key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    nodes: BTreeMap<String, NodeId>,
    /// The node of version 1 of the state file, in its one space.
    #[serde(default, skip_serializing)]
    node: Option<NodeId>,
    /// Whether the project filter, or the routing, left the file out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    excluded: bool,
}

/// The state file: every session file read so far.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    version: u32,
    /// The id of the one space of version 1 of the state file.
    #[serde(default, skip_serializing)]
    space: Option<String>,
    /// The projects the filter left out when the files were read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    excluded: Vec<PathBuf>,
    /// The routing of the destinations when the files were read ([`Destinations::routing`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    routing: Option<String>,
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

impl State {
    /// Reads a state file, moving the nodes of version 1 to the space it was written for.
    fn parse(bytes: &[u8]) -> Option<State> {
        let mut state = serde_json::from_slice::<State>(bytes).ok()?;
        match state.version {
            STATE_VERSION => {}
            1 => {
                let space = state.space.take();
                for file in state.files.values_mut() {
                    if let (Some(space), Some(node)) = (&space, file.node.take()) {
                        file.nodes.insert(space.clone(), node);
                    }
                }
                state.version = STATE_VERSION;
            }
            _ => return None,
        }
        Some(state)
    }
}

/// Where captured conversations go: the spaces each conversation is written to.
///
/// [`Capture::sync_all`] and [`Capture::sync_paths`] write every conversation to one space.
/// [`Capture::sync_all_to`] and [`Capture::sync_paths_to`] ask a `Destinations` instead, for
/// each conversation, as Knode Service does from the folders of every space it knows.
pub trait Destinations {
    /// Text that changes whenever [`Destinations::route`] may answer differently. Files whose
    /// conversation went to no space are read again when it changes.
    fn routing(&self) -> String;

    /// The spaces a conversation read from the session file `file` goes to, by key (such as
    /// the space id). Empty when it is not stored anywhere: then nothing of it is written, and
    /// its text is not kept in the state file either.
    fn route(&self, conversation: &Conversation, file: &Path) -> Vec<String>;

    /// Runs `write` with the space of this key, one of those [`Destinations::route`] gave.
    fn with_space(
        &self,
        key: &str,
        write: &mut dyn FnMut(&mut Space) -> Result<(), CaptureError>,
    ) -> Result<(), CaptureError>;
}

/// One space that every conversation goes to, keyed by its id.
struct OneSpace<'a> {
    id: String,
    space: RefCell<&'a mut Space>,
}

impl<'a> OneSpace<'a> {
    fn new(space: &'a mut Space) -> Self {
        OneSpace {
            id: space.manifest().id.clone(),
            space: RefCell::new(space),
        }
    }
}

impl Destinations for OneSpace<'_> {
    fn routing(&self) -> String {
        self.id.clone()
    }

    fn route(&self, _: &Conversation, _: &Path) -> Vec<String> {
        vec![self.id.clone()]
    }

    fn with_space(
        &self,
        _: &str,
        write: &mut dyn FnMut(&mut Space) -> Result<(), CaptureError>,
    ) -> Result<(), CaptureError> {
        write(&mut self.space.borrow_mut())
    }
}

impl Capture {
    /// Capture with its state in `state_path`, read now if it exists, and the default session
    /// folders of every agent (see [`default_roots`](crate::default_roots)).
    ///
    /// A state file that cannot be parsed, or that was written by a newer version, is ignored:
    /// every session is read again from the start, which finds and updates the same nodes.
    pub fn open(state_path: impl Into<PathBuf>) -> Result<Self, CaptureError> {
        let state_path = state_path.into();
        let state = match fs::read(&state_path) {
            Ok(bytes) => State::parse(&bytes).unwrap_or_default(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => State::default(),
            Err(error) => return Err(CaptureError::State(error)),
        };
        let roots = Agent::ALL
            .into_iter()
            .flat_map(|agent| {
                default_roots(agent)
                    .into_iter()
                    .map(move |root| (agent, root))
            })
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

    /// Reads every session in the session folders and writes what changed, then saves the
    /// state. Use it once at start, to catch up, and from time to time to find what the
    /// watcher missed. Files that are gone are forgotten; their nodes stay. A session folder
    /// that cannot be listed (or a database that cannot be read) is reported in
    /// [`CaptureReport::failed`] and the others are still read.
    pub fn sync_all(&mut self, space: &mut Space) -> Result<CaptureReport, CaptureError> {
        self.sync_all_to(&OneSpace::new(space))
    }

    /// [`Capture::sync_all`], writing each conversation to the spaces `destinations` route it
    /// to.
    pub fn sync_all_to(
        &mut self,
        destinations: &dyn Destinations,
    ) -> Result<CaptureReport, CaptureError> {
        let mut files = Vec::new();
        let mut unlisted = Vec::new();
        for (agent, root) in &self.roots {
            match discover(*agent, root) {
                Ok(found) => files.extend(found),
                Err(error) => unlisted.push((root.clone(), error.to_string())),
            }
        }
        let present: std::collections::BTreeSet<&Path> =
            files.iter().map(|f| f.path.as_path()).collect();
        let roots = &self.roots;
        self.state.files.retain(|path, _| {
            present.contains(path.as_path())
                || unlisted.iter().any(|(root, _)| path.starts_with(root))
                || !roots.iter().any(|(_, root)| path.starts_with(root))
        });
        let mut report = self.sync_files(destinations, &files)?;
        report.failed.extend(unlisted);
        Ok(report)
    }

    /// Reads the given paths, such as the ones a [`Watcher`](crate::Watcher) reported, and
    /// writes what changed, then saves the state. Paths that are not session files under one of
    /// the session folders are ignored; removed files are forgotten. A change to a database
    /// (OpenCode, Cursor, or a Windsurf window) checks every session in it, and reads the ones
    /// that changed.
    pub fn sync_paths<P: AsRef<Path>>(
        &mut self,
        space: &mut Space,
        paths: &[P],
    ) -> Result<CaptureReport, CaptureError> {
        self.sync_paths_to(&OneSpace::new(space), paths)
    }

    /// [`Capture::sync_paths`], writing each conversation to the spaces `destinations` route it
    /// to.
    pub fn sync_paths_to<P: AsRef<Path>>(
        &mut self,
        destinations: &dyn Destinations,
        paths: &[P],
    ) -> Result<CaptureReport, CaptureError> {
        let mut files = Vec::new();
        let mut unlisted = Vec::new();
        for path in paths {
            let Some((agent, root, path)) = self.locate(path.as_ref()) else {
                continue;
            };
            match changed_sessions(agent, &root, &path) {
                Ok(found) if !found.is_empty() => files.extend(found),
                Ok(_) => {
                    if !path.exists() {
                        self.state.files.remove(&path);
                    }
                }
                Err(error) => unlisted.push((path, error.to_string())),
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files.dedup_by(|a, b| a.path == b.path);
        let mut report = self.sync_files(destinations, &files)?;
        report.failed.extend(unlisted);
        Ok(report)
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
        destinations: &dyn Destinations,
        files: &[SessionFile],
    ) -> Result<CaptureReport, CaptureError> {
        self.state.version = STATE_VERSION;
        let routing = destinations.routing();
        if self.state.excluded != self.filter.excluded()
            || self.state.routing.as_deref() != Some(routing.as_str())
        {
            // The filter or the routing changed: what was left out before is read again.
            self.state.files.retain(|_, file| !file.excluded);
            self.state.excluded = self.filter.excluded().to_vec();
            self.state.routing = Some(routing);
        }
        let mut report = CaptureReport::default();
        let mut outcome = Ok(());
        for file in files {
            report.files += 1;
            if let Err(error) = self.sync_file(destinations, file, &mut report) {
                outcome = Err(error);
                break;
            }
        }
        // Save what was done even when a write failed, so it is not done twice.
        self.save()?;
        outcome.map(|()| report)
    }

    /// Remembers that a file was left out, without its text.
    fn leave_out(&mut self, file: &SessionFile, offset: u64, report: &mut CaptureReport) {
        self.state.files.insert(
            file.path.clone(),
            FileState {
                agent: file.agent,
                offset,
                conversation: None,
                nodes: BTreeMap::new(),
                node: None,
                excluded: true,
            },
        );
        report.filtered += 1;
    }

    fn sync_file(
        &mut self,
        destinations: &dyn Destinations,
        file: &SessionFile,
        report: &mut CaptureReport,
    ) -> Result<(), CaptureError> {
        if !self.filter.may_allow_file(file) {
            self.leave_out(file, file.len, report);
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
                self.leave_out(file, file.len, report);
                return Ok(());
            }
            if state.offset == file.len {
                if let Some(conversation) = &state.conversation {
                    let targets = destinations.route(conversation, &file.path);
                    if targets.is_empty() {
                        // The routing changed: it goes nowhere now, and its text is dropped.
                        self.leave_out(file, file.len, report);
                        return Ok(());
                    }
                    if targets.iter().all(|key| state.nodes.contains_key(key)) {
                        report.unchanged += 1;
                        return Ok(());
                    }
                }
            }
        }
        let previous_session = previous
            .as_ref()
            .and_then(|state| state.conversation.as_ref())
            .map(|conversation| conversation.session_id.clone());
        let mut nodes = previous
            .as_ref()
            .map(|state| state.nodes.clone())
            .unwrap_or_default();
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
        // The sessions that ended in the file (an Aider history file holds one per run) are
        // written once, as they end.
        for finished in reader.take_finished() {
            if finished.is_empty() || !self.filter.allows(&finished) {
                continue;
            }
            let same = previous_session.as_deref() == Some(finished.session_id.as_str());
            for key in destinations.route(&finished, &file.path) {
                let known = if same { nodes.get(&key).copied() } else { None };
                if let Written::Changed(captured) =
                    write_to(destinations, &key, &finished, &file.path, known)?
                {
                    report.changes.push(captured);
                }
            }
        }
        let conversation = reader.conversation();
        let offset = reader.offset();
        if previous_session.is_some_and(|id| id != conversation.session_id) {
            // The file moved on to another session: its nodes are not this one's.
            nodes.clear();
        }

        let targets = if self.filter.allows(conversation) {
            destinations.route(conversation, &file.path)
        } else {
            Vec::new()
        };
        if targets.is_empty() {
            self.leave_out(file, offset, report);
            return Ok(());
        }
        if conversation.is_empty() {
            report.empty += 1;
        } else {
            for key in targets {
                let known = nodes.get(&key).copied();
                if progress.records == 0 && known.is_some() {
                    report.unchanged += 1;
                    continue;
                }
                match write_to(destinations, &key, conversation, &file.path, known)? {
                    Written::Changed(captured) => {
                        nodes.insert(key, captured.node_id);
                        report.changes.push(captured);
                    }
                    Written::Unchanged(id) => {
                        nodes.insert(key, id);
                        report.unchanged += 1;
                    }
                    Written::Deleted => report.deleted += 1,
                }
            }
        }
        let state = FileState {
            agent: file.agent,
            offset,
            conversation: Some(conversation.clone()),
            nodes,
            node: None,
            excluded: false,
        };
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

/// Writes a conversation into the space of `key`.
fn write_to(
    destinations: &dyn Destinations,
    key: &str,
    conversation: &Conversation,
    path: &Path,
    known: Option<NodeId>,
) -> Result<Written, CaptureError> {
    let mut written = None;
    destinations.with_space(key, &mut |space| {
        written = Some(write_conversation(space, key, conversation, path, known)?);
        Ok(())
    })?;
    written.ok_or_else(|| {
        CaptureError::Space(knode_format::Error::Invalid(format!(
            "no space to write the conversation to: {key}"
        )))
    })
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
pub(crate) fn find_conversation(
    space: &Space,
    agent: Agent,
    session_id: &str,
) -> Result<Option<NodeId>, knode_format::Error> {
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
    key: &str,
    conversation: &Conversation,
    path: &Path,
    known: Option<NodeId>,
) -> Result<Written, CaptureError> {
    let by = Provenance::agent(conversation.agent.id())
        .with_session(&conversation.session_id)
        .with_source(path.to_string_lossy());
    let captured = |node_id, op| Captured {
        space: key.to_string(),
        node_id,
        op,
        agent: conversation.agent,
        session_id: conversation.session_id.clone(),
        path: path.to_path_buf(),
    };
    let existing = find_conversation(space, conversation.agent, &conversation.session_id)?;
    if existing.is_none() && known.is_some() {
        // Written before and deleted since: the person removed it on purpose.
        return Ok(Written::Deleted);
    }
    // One history row per run of updates from the session: the transcript only grows, so the
    // newest version holds every earlier one.
    Ok(
        match upsert_conversation(space, conversation, existing, &by, true)? {
            Upserted::Created(id) => Written::Changed(captured(id, ChangeOp::Create)),
            Upserted::Updated(id) => Written::Changed(captured(id, ChangeOp::Update)),
            Upserted::Unchanged(id) => Written::Unchanged(id),
        },
    )
}

/// What [`upsert_conversation`] did.
pub(crate) enum Upserted {
    Created(NodeId),
    Updated(NodeId),
    Unchanged(NodeId),
}

/// Writes a conversation into its node `existing`, or into a new `conversation` node when there
/// is none: the title of [`node_title`], the body of [`to_markdown`] and the properties of
/// [`conversation_properties`]. Text updates are coalesced into the previous history row when
/// `coalesce` is set.
pub(crate) fn upsert_conversation(
    space: &mut Space,
    conversation: &Conversation,
    existing: Option<NodeId>,
    by: &Provenance,
    coalesce: bool,
) -> Result<Upserted, knode_format::Error> {
    let title = node_title(conversation);
    let body = to_markdown(conversation);
    let properties = conversation_properties(conversation);
    let Some(id) = existing else {
        let node = space.create_node_with_properties(
            NewNode::note(title, body).with_kind(kinds::CONVERSATION),
            &properties,
            by,
        )?;
        return Ok(Upserted::Created(node.id));
    };
    let current = space
        .get_node(&id)?
        .ok_or(knode_format::Error::NotFound(id))?;
    let text_changed = current.title != title || current.body != body;
    if text_changed {
        let update = NodeUpdate {
            title: Some(title),
            body: Some(body),
            ..Default::default()
        };
        if coalesce {
            space.update_node_coalescing(&id, update, by)?;
        } else {
            space.update_node(&id, update, by)?;
        }
    }
    let properties_changed = space.set_properties(&id, &properties, by)?;
    Ok(if text_changed || properties_changed {
        Upserted::Updated(id)
    } else {
        Upserted::Unchanged(id)
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
