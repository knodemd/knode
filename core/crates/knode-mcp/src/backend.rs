//! Where the MCP tools read and write: a [`Backend`].
//!
//! [`DirectBackend`] opens the space itself with `knode-format`.
//! [`ServiceBackend`](crate::ServiceBackend) talks to Knode Service over its socket and
//! implements the same trait, so the tools do not change.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use knode_format::{
    Attachment, CanvasBoard, Change, ChangeDetail, ChangeId, HistoryEntry, HybridHit, Link,
    LinkKind, LinkKindSpec, NewNode, Node, NodeId, NodeLink, NodeSummary, NodeUpdate, Provenance,
    SearchMode, Space, View, rrf_score, view_kinds,
};
use knode_sync::{BodyEdit, Docs, Splice, Version};
use serde::Serialize;

/// What can go wrong in a backend. Every error is shown to the agent as a tool error, so the
/// message must say what to do about it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    /// The request broke a rule: a missing node, an empty title, a bad kind.
    #[error("{0}")]
    Invalid(String),
    /// The space cannot be reached: none is configured, or it cannot be opened.
    #[error("{0}")]
    Unavailable(String),
    /// Anything else, such as a database error.
    #[error("{0}")]
    Failed(String),
}

impl From<knode_format::Error> for BackendError {
    fn from(error: knode_format::Error) -> Self {
        match error {
            knode_format::Error::Invalid(_)
            | knode_format::Error::NotFound(_)
            | knode_format::Error::ChangeNotFound(_)
            | knode_format::Error::CannotUndo { .. }
            | knode_format::Error::AlreadyUndone { .. }
            | knode_format::Error::UndoConflict { .. }
            | knode_format::Error::LinkKindInUse { .. }
            | knode_format::Error::PersonOnly(_) => BackendError::Invalid(error.to_string()),
            other => BackendError::Failed(other.to_string()),
        }
    }
}

impl From<knode_sync::Error> for BackendError {
    fn from(error: knode_sync::Error) -> Self {
        match error {
            knode_sync::Error::Format(e) => e.into(),
            knode_sync::Error::NotFound(_) | knode_sync::Error::Invalid(_) => {
                BackendError::Invalid(error.to_string())
            }
            knode_sync::Error::UnknownVersion(_) => BackendError::Invalid(format!(
                "{error}. Read the node again with read_node and pass its version"
            )),
            other => BackendError::Failed(other.to_string()),
        }
    }
}

/// A node's body opened for editing together: its text and the version of its document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyDoc {
    /// The body.
    pub text: String,
    /// The version of the body's document: pass it back when writing what was read.
    pub version: String,
}

/// What writing a node did.
#[derive(Debug, Clone, PartialEq)]
pub struct Written {
    /// The node as it is now.
    pub node: Node,
    /// Whether others had changed the body since the version the edit was based on, and the edit
    /// was merged with their changes instead of replacing them.
    pub merged: bool,
}

/// The versions an agent has read, by node, so a body it rewrites later is merged from what it
/// read even when it does not pass the version back.
#[derive(Debug, Default)]
pub(crate) struct ReadVersions(Mutex<HashMap<NodeId, String>>);

impl ReadVersions {
    pub(crate) fn get(&self, id: &NodeId) -> Option<String> {
        self.lock().get(id).cloned()
    }

    pub(crate) fn set(&self, id: &NodeId, version: String) {
        self.lock().insert(*id, version);
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<NodeId, String>> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// The splice that appends `extra` to `text` after a blank line, or replaces a blank text: in
/// characters, as [`Splice`] counts them.
pub(crate) fn append_splice(text: &str, extra: &str) -> Splice {
    let len = text.chars().count();
    if text.trim().is_empty() {
        return Splice {
            at: 0,
            delete: len,
            insert: extra.to_string(),
        };
    }
    let kept = text.trim_end().chars().count();
    Splice {
        at: kept,
        delete: len - kept,
        insert: format!("\n\n{extra}"),
    }
}

/// A `Result` whose error is [`BackendError`].
pub type BackendResult<T> = Result<T, BackendError>;

/// What a search found, and how.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchResults {
    /// The mode the search ran in: `words` when meaning was asked for but cannot be used.
    pub mode: SearchMode,
    /// The results, best first.
    pub hits: Vec<HybridHit>,
    /// Why the search ran by words, or that some notes were found by words only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl SearchResults {
    /// Results of a search by words, as hybrid hits ranked by words alone.
    pub fn by_words(hits: Vec<knode_format::SearchHit>, note: Option<String>) -> Self {
        let hits = hits
            .into_iter()
            .enumerate()
            .map(|(index, hit)| {
                let rank = Some(index as u32 + 1);
                HybridHit {
                    node: hit.node,
                    snippet: hit.snippet,
                    score: rrf_score(rank, None),
                    word_rank: rank,
                    meaning_rank: None,
                }
            })
            .collect();
        SearchResults {
            mode: SearchMode::Words,
            hits,
            note,
        }
    }
}

/// The operations the MCP tools need from a space. The methods mirror [`Space`] and the
/// Knode Service methods of the same name.
pub trait Backend: Send + Sync + 'static {
    /// `search`: nodes matching the words, the meaning or both, best first. A backend that
    /// cannot search by meaning searches by words and says so in the note.
    fn search(&self, query: &str, mode: SearchMode, limit: usize) -> BackendResult<SearchResults>;
    /// `node.get`: one node, or `None`.
    fn get_node(&self, id: &NodeId) -> BackendResult<Option<Node>>;
    /// The nodes with this title, oldest first.
    fn find_by_title(&self, title: &str) -> BackendResult<Vec<NodeSummary>>;
    /// `node.create`.
    fn create_node(&self, new: NewNode, by: &Provenance) -> BackendResult<Node>;
    /// `node.update`.
    fn update_node(&self, id: &NodeId, update: NodeUpdate, by: &Provenance) -> BackendResult<Node>;
    /// `link.create`.
    fn link(&self, from: &NodeId, to: &NodeId, kind: &str, by: &Provenance) -> BackendResult<Link>;
    /// `links.from`.
    fn links_from(&self, id: &NodeId) -> BackendResult<Vec<Link>>;
    /// `links.back`.
    fn backlinks(&self, id: &NodeId) -> BackendResult<Vec<Link>>;
    /// `recent`: the nodes changed last, newest first.
    fn recent(&self, limit: usize) -> BackendResult<Vec<NodeSummary>>;
    /// `history`: every version of a node, oldest first.
    fn history(&self, id: &NodeId) -> BackendResult<Vec<HistoryEntry>>;
    /// `changes`: the changes of the space, newest first, older than `before` when set.
    fn changes(&self, limit: usize, before: Option<ChangeId>) -> BackendResult<Vec<Change>>;
    /// `changes` with a subject: the changes made to a node, newest first.
    fn changes_of(&self, subject: &str, limit: usize) -> BackendResult<Vec<Change>>;
    /// `change`: one change with what it wrote, or `None`.
    fn change(&self, id: ChangeId) -> BackendResult<Option<ChangeDetail>>;
    /// `undo`: undoes a change and returns the change that undoes it.
    fn undo(&self, id: ChangeId, force: bool, by: &Provenance) -> BackendResult<Change>;
    /// `restore`: puts a node back to one of its versions and returns it.
    fn restore(&self, id: &NodeId, version: i64, by: &Provenance) -> BackendResult<Node>;
    /// Whether the backend knows which agent is calling from its key, and records and checks
    /// its changes under that name whatever name the MCP client gives. Knode Service does; a
    /// space opened directly does not.
    fn knows_the_agent(&self) -> bool {
        false
    }
    /// `doc.open`: a node's body and the version of its document, remembered as the version
    /// this agent read. `None` from a backend that does not merge bodies.
    fn open_body(&self, _id: &NodeId) -> BackendResult<Option<BodyDoc>> {
        Ok(None)
    }
    /// `node.update` with the version of the body read: a new body is merged with what others
    /// wrote since `version` (or since the version this agent last read, when `None`) instead of
    /// replacing it.
    fn write_body(
        &self,
        id: &NodeId,
        update: NodeUpdate,
        _version: Option<&str>,
        by: &Provenance,
    ) -> BackendResult<Written> {
        Ok(Written {
            node: self.update_node(id, update, by)?,
            merged: false,
        })
    }
    /// Adds `extra` to the end of a node's body, after a blank line, merged with what others
    /// write at the same time.
    fn append_body(&self, id: &NodeId, extra: &str, by: &Provenance) -> BackendResult<Written> {
        let current = self
            .get_node(id)?
            .ok_or_else(|| BackendError::Invalid(format!("no node with id {id}")))?;
        let splice = append_splice(&current.body, extra);
        let mut body: Vec<char> = current.body.chars().collect();
        body.splice(splice.at..splice.at + splice.delete, splice.insert.chars());
        let update = NodeUpdate {
            body: Some(body.into_iter().collect()),
            ..Default::default()
        };
        Ok(Written {
            node: self.update_node(id, update, by)?,
            merged: false,
        })
    }
    /// `link_kinds.list`: every kind of link the space knows.
    fn link_kinds(&self) -> BackendResult<Vec<LinkKind>>;
    /// `link_kind.set`: defines a kind of link, or changes its labels.
    fn set_link_kind(&self, spec: &LinkKindSpec, by: &Provenance) -> BackendResult<LinkKind>;
    /// `link_kind.remove`: removes a kind of link the space defined.
    fn remove_link_kind(&self, kind: &str, by: &Provenance) -> BackendResult<bool>;
    /// `links.of`: the links of a node, going out then coming in, with their labels.
    fn node_links(&self, id: &NodeId) -> BackendResult<Vec<NodeLink>>;
    /// `views.list`: the graph views and canvases, a canvas named by its node's title.
    fn views(&self) -> BackendResult<Vec<View>>;
    /// `canvas.board`: a canvas with its nodes, groups and links, or `None`.
    fn canvas_board(&self, node: &NodeId) -> BackendResult<Option<CanvasBoard>>;
    /// `attachments.list`: the files attached to a node.
    fn attachments(&self, id: &NodeId) -> BackendResult<Vec<Attachment>>;
    /// `attachment.read`: an attachment with its bytes, or `None`.
    fn read_attachment(
        &self,
        id: &NodeId,
        name: &str,
    ) -> BackendResult<Option<(Attachment, Vec<u8>)>>;
    /// `attachment.add`: attaches bytes to a node under a name.
    fn attach(
        &self,
        id: &NodeId,
        name: &str,
        bytes: &[u8],
        by: &Provenance,
    ) -> BackendResult<Attachment>;
    /// `attachment.add_file`: attaches a file on this machine. Only a person may, through Knode
    /// Service.
    fn attach_file(&self, id: &NodeId, path: &Path, by: &Provenance) -> BackendResult<Attachment>;
    /// `folders.list`: the folders that feed the space, where they are on this machine.
    fn folders(&self) -> BackendResult<Vec<LocalFolder>>;
}

/// A folder that feeds the space, as this machine sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFolder {
    /// `project`, `notes` or `agent`.
    pub kind: String,
    /// Whether the folder is captured and read.
    pub capture: bool,
    /// The folder on this machine, or `None` when its path cannot be resolved here.
    pub path: Option<PathBuf>,
}

/// A folder recorded as `~/...` or absolute, on this machine.
pub(crate) fn resolve_folder(path: &str) -> Option<PathBuf> {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
        return Some(PathBuf::from(home).join(rest));
    }
    let path = PathBuf::from(path);
    path.is_absolute().then_some(path)
}

/// A view with a canvas named by its node's title.
fn named(space: &Space, mut view: View) -> knode_format::Result<View> {
    if view.kind == view_kinds::CANVAS
        && let Some(node) = view.node_id
        && let Some(found) = space.get_node(&node)?
    {
        view.name = found.title;
    }
    Ok(view)
}

/// A backend that opens the space directly with `knode-format`.
///
/// The space is opened on first use and kept open. If it cannot be opened, each call says why
/// and the next call tries again, so the server keeps running while the person fixes it.
#[derive(Debug)]
pub struct DirectBackend {
    target: Target,
    space: Mutex<Option<Space>>,
    /// The Loro documents of the bodies this agent edits; locked after the space.
    docs: Mutex<Docs>,
    read: ReadVersions,
}

#[derive(Debug)]
enum Target {
    Path(PathBuf),
    Missing(String),
}

impl DirectBackend {
    /// A backend for the space at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        DirectBackend {
            target: Target::Path(path.into()),
            space: Mutex::new(None),
            docs: Mutex::default(),
            read: ReadVersions::default(),
        }
    }

    /// A backend with no space, whose every call fails with `message`, such as how to create
    /// a space. The server still starts, so the agent can tell the person what to do.
    pub fn unavailable(message: impl Into<String>) -> Self {
        DirectBackend {
            target: Target::Missing(message.into()),
            space: Mutex::new(None),
            docs: Mutex::default(),
            read: ReadVersions::default(),
        }
    }

    /// A backend for a space that is already open.
    pub fn from_space(space: Space) -> Self {
        DirectBackend {
            target: Target::Path(space.path().to_path_buf()),
            space: Mutex::new(Some(space)),
            docs: Mutex::default(),
            read: ReadVersions::default(),
        }
    }

    /// The space this backend opens, if it has one.
    pub fn path(&self) -> Option<&Path> {
        match &self.target {
            Target::Path(path) => Some(path),
            Target::Missing(_) => None,
        }
    }

    pub(crate) fn with<T>(
        &self,
        f: impl FnOnce(&mut Space) -> knode_format::Result<T>,
    ) -> BackendResult<T> {
        self.with_space(|space| Ok(f(space)?))
    }

    /// Runs `f` with the space and the Loro documents.
    fn with_docs<T>(
        &self,
        f: impl FnOnce(&mut Space, &mut Docs) -> BackendResult<T>,
    ) -> BackendResult<T> {
        self.with_space(|space| {
            let mut docs = self.docs.lock().unwrap_or_else(|p| p.into_inner());
            f(space, &mut docs)
        })
    }

    fn with_space<T>(&self, f: impl FnOnce(&mut Space) -> BackendResult<T>) -> BackendResult<T> {
        let path = match &self.target {
            Target::Path(path) => path,
            Target::Missing(message) => return Err(BackendError::Unavailable(message.clone())),
        };
        let mut guard: MutexGuard<'_, Option<Space>> = self
            .space
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            let space = Space::open(path).map_err(|e| {
                BackendError::Unavailable(format!(
                    "cannot open the Knode space at {}: {e}",
                    path.display()
                ))
            })?;
            *guard = Some(space);
        }
        let space = guard.as_mut().expect("the space was just opened");
        f(space)
    }
}

impl Backend for DirectBackend {
    fn search(&self, query: &str, mode: SearchMode, limit: usize) -> BackendResult<SearchResults> {
        let hits = self.with(|s| s.search(query, limit))?;
        let note = (mode != SearchMode::Words)
            .then(|| "Knode Service is not running, so this search was by words only.".to_string());
        Ok(SearchResults::by_words(hits, note))
    }

    fn get_node(&self, id: &NodeId) -> BackendResult<Option<Node>> {
        self.with(|s| s.get_node(id))
    }

    fn find_by_title(&self, title: &str) -> BackendResult<Vec<NodeSummary>> {
        self.with(|s| s.find_by_title(title))
    }

    fn create_node(&self, new: NewNode, by: &Provenance) -> BackendResult<Node> {
        self.with(|s| s.create_node(new, by))
    }

    fn update_node(&self, id: &NodeId, update: NodeUpdate, by: &Provenance) -> BackendResult<Node> {
        self.with(|s| s.update_node(id, update, by))
    }

    fn link(&self, from: &NodeId, to: &NodeId, kind: &str, by: &Provenance) -> BackendResult<Link> {
        self.with(|s| s.link(from, to, kind, by))
    }

    fn links_from(&self, id: &NodeId) -> BackendResult<Vec<Link>> {
        self.with(|s| s.links_from(id))
    }

    fn backlinks(&self, id: &NodeId) -> BackendResult<Vec<Link>> {
        self.with(|s| s.backlinks(id))
    }

    fn recent(&self, limit: usize) -> BackendResult<Vec<NodeSummary>> {
        self.with(|s| s.recent(limit))
    }

    fn history(&self, id: &NodeId) -> BackendResult<Vec<HistoryEntry>> {
        self.with(|s| s.history(id))
    }

    fn changes(&self, limit: usize, before: Option<ChangeId>) -> BackendResult<Vec<Change>> {
        self.with(|s| s.changes(limit, before))
    }

    fn changes_of(&self, subject: &str, limit: usize) -> BackendResult<Vec<Change>> {
        self.with(|s| s.changes_of(subject, limit))
    }

    fn change(&self, id: ChangeId) -> BackendResult<Option<ChangeDetail>> {
        self.with(|s| s.change(id))
    }

    fn undo(&self, id: ChangeId, force: bool, by: &Provenance) -> BackendResult<Change> {
        self.with(|s| s.undo(id, by, force))
    }

    fn restore(&self, id: &NodeId, version: i64, by: &Provenance) -> BackendResult<Node> {
        self.with(|s| s.restore(id, version, by))
    }

    fn open_body(&self, id: &NodeId) -> BackendResult<Option<BodyDoc>> {
        let open = self.with_docs(|space, docs| Ok(docs.open_doc(space, id)?))?;
        let version = open.version.to_json();
        self.read.set(id, version.clone());
        Ok(Some(BodyDoc {
            text: open.text,
            version,
        }))
    }

    fn write_body(
        &self,
        id: &NodeId,
        mut update: NodeUpdate,
        version: Option<&str>,
        by: &Provenance,
    ) -> BackendResult<Written> {
        let base = match version.map(str::to_string).or_else(|| self.read.get(id)) {
            Some(version) => Some(Version::from_json(&version).map_err(|_| {
                BackendError::Invalid(format!(
                    "{version} is not a version read_node gave. Read the node again and pass its \
                     version, or leave version out"
                ))
            })?),
            None => None,
        };
        let Some(body) = update.body.take() else {
            return Ok(Written {
                node: self.update_node(id, update, by)?,
                merged: false,
            });
        };
        let written = self.with_docs(|space, docs| {
            // The short fields first: a bad title fails before the body is written.
            space.update_node(id, update, by)?;
            let has_doc = Docs::has_doc(space, id);
            if base.is_none() && !has_doc {
                let update = NodeUpdate {
                    body: Some(body),
                    ..Default::default()
                };
                return Ok((space.update_node(id, update, by)?, None));
            }
            let edit = match base {
                Some(base) => BodyEdit::text(body).based_on(base),
                None => BodyEdit::text(body),
            };
            let written = docs.apply_local_edit(space, id, edit, by)?;
            Ok((written.node, Some((written.version, written.seen))))
        })?;
        Ok(match written {
            (node, Some((version, seen))) => {
                self.read.set(id, seen.to_json());
                Written {
                    node,
                    merged: version != seen,
                }
            }
            (node, None) => Written {
                node,
                merged: false,
            },
        })
    }

    fn append_body(&self, id: &NodeId, extra: &str, by: &Provenance) -> BackendResult<Written> {
        let written = self.with_docs(|space, docs| {
            let open = docs.open_doc(space, id)?;
            let splice = append_splice(&open.text, extra);
            let edit = BodyEdit::splices(vec![splice]).based_on(open.version);
            Ok(docs.apply_local_edit(space, id, edit, by)?)
        })?;
        Ok(Written {
            node: written.node,
            merged: false,
        })
    }

    fn link_kinds(&self) -> BackendResult<Vec<LinkKind>> {
        self.with(|s| s.link_kinds())
    }

    fn set_link_kind(&self, spec: &LinkKindSpec, by: &Provenance) -> BackendResult<LinkKind> {
        self.with(|s| s.set_link_kind(spec, by))
    }

    fn remove_link_kind(&self, kind: &str, by: &Provenance) -> BackendResult<bool> {
        self.with(|s| s.remove_link_kind(kind, by))
    }

    fn node_links(&self, id: &NodeId) -> BackendResult<Vec<NodeLink>> {
        self.with(|s| s.node_links(id))
    }

    fn views(&self) -> BackendResult<Vec<View>> {
        self.with(|s| {
            let views = s.views()?;
            views.into_iter().map(|v| named(s, v)).collect()
        })
    }

    fn canvas_board(&self, node: &NodeId) -> BackendResult<Option<CanvasBoard>> {
        self.with(|s| {
            Ok(s.canvas_board(node)?.map(|mut board| {
                board.view.name = board.title.clone();
                board
            }))
        })
    }

    fn attachments(&self, id: &NodeId) -> BackendResult<Vec<Attachment>> {
        self.with(|s| s.attachments(id))
    }

    fn read_attachment(
        &self,
        id: &NodeId,
        name: &str,
    ) -> BackendResult<Option<(Attachment, Vec<u8>)>> {
        self.with(|s| {
            let Some(attachment) = s.attachment(id, name)? else {
                return Ok(None);
            };
            let bytes = s.read_blob(&attachment.hash)?.ok_or_else(|| {
                knode_format::Error::Invalid(format!(
                    "the file of {} is not on this machine yet",
                    attachment.name
                ))
            })?;
            Ok(Some((attachment, bytes)))
        })
    }

    fn attach(
        &self,
        id: &NodeId,
        name: &str,
        bytes: &[u8],
        by: &Provenance,
    ) -> BackendResult<Attachment> {
        self.with(|s| s.attach(id, name, bytes, by))
    }

    fn attach_file(&self, id: &NodeId, path: &Path, by: &Provenance) -> BackendResult<Attachment> {
        self.with(|s| s.attach_file(id, path, by))
    }

    fn folders(&self) -> BackendResult<Vec<LocalFolder>> {
        self.with(|s| {
            Ok(s.folders()?
                .into_iter()
                .map(|f| LocalFolder {
                    path: resolve_folder(&f.path),
                    kind: f.kind,
                    capture: f.capture,
                })
                .collect())
        })
    }
}

/// A boxed backend is a backend, so the backend can be picked at run time.
impl<B: Backend + ?Sized> Backend for Box<B> {
    fn search(&self, query: &str, mode: SearchMode, limit: usize) -> BackendResult<SearchResults> {
        (**self).search(query, mode, limit)
    }

    fn get_node(&self, id: &NodeId) -> BackendResult<Option<Node>> {
        (**self).get_node(id)
    }

    fn find_by_title(&self, title: &str) -> BackendResult<Vec<NodeSummary>> {
        (**self).find_by_title(title)
    }

    fn create_node(&self, new: NewNode, by: &Provenance) -> BackendResult<Node> {
        (**self).create_node(new, by)
    }

    fn update_node(&self, id: &NodeId, update: NodeUpdate, by: &Provenance) -> BackendResult<Node> {
        (**self).update_node(id, update, by)
    }

    fn link(&self, from: &NodeId, to: &NodeId, kind: &str, by: &Provenance) -> BackendResult<Link> {
        (**self).link(from, to, kind, by)
    }

    fn links_from(&self, id: &NodeId) -> BackendResult<Vec<Link>> {
        (**self).links_from(id)
    }

    fn backlinks(&self, id: &NodeId) -> BackendResult<Vec<Link>> {
        (**self).backlinks(id)
    }

    fn recent(&self, limit: usize) -> BackendResult<Vec<NodeSummary>> {
        (**self).recent(limit)
    }

    fn history(&self, id: &NodeId) -> BackendResult<Vec<HistoryEntry>> {
        (**self).history(id)
    }

    fn changes(&self, limit: usize, before: Option<ChangeId>) -> BackendResult<Vec<Change>> {
        (**self).changes(limit, before)
    }

    fn changes_of(&self, subject: &str, limit: usize) -> BackendResult<Vec<Change>> {
        (**self).changes_of(subject, limit)
    }

    fn change(&self, id: ChangeId) -> BackendResult<Option<ChangeDetail>> {
        (**self).change(id)
    }

    fn undo(&self, id: ChangeId, force: bool, by: &Provenance) -> BackendResult<Change> {
        (**self).undo(id, force, by)
    }

    fn restore(&self, id: &NodeId, version: i64, by: &Provenance) -> BackendResult<Node> {
        (**self).restore(id, version, by)
    }

    fn knows_the_agent(&self) -> bool {
        (**self).knows_the_agent()
    }

    fn open_body(&self, id: &NodeId) -> BackendResult<Option<BodyDoc>> {
        (**self).open_body(id)
    }

    fn write_body(
        &self,
        id: &NodeId,
        update: NodeUpdate,
        version: Option<&str>,
        by: &Provenance,
    ) -> BackendResult<Written> {
        (**self).write_body(id, update, version, by)
    }

    fn append_body(&self, id: &NodeId, extra: &str, by: &Provenance) -> BackendResult<Written> {
        (**self).append_body(id, extra, by)
    }

    fn link_kinds(&self) -> BackendResult<Vec<LinkKind>> {
        (**self).link_kinds()
    }

    fn set_link_kind(&self, spec: &LinkKindSpec, by: &Provenance) -> BackendResult<LinkKind> {
        (**self).set_link_kind(spec, by)
    }

    fn remove_link_kind(&self, kind: &str, by: &Provenance) -> BackendResult<bool> {
        (**self).remove_link_kind(kind, by)
    }

    fn node_links(&self, id: &NodeId) -> BackendResult<Vec<NodeLink>> {
        (**self).node_links(id)
    }

    fn views(&self) -> BackendResult<Vec<View>> {
        (**self).views()
    }

    fn canvas_board(&self, node: &NodeId) -> BackendResult<Option<CanvasBoard>> {
        (**self).canvas_board(node)
    }

    fn attachments(&self, id: &NodeId) -> BackendResult<Vec<Attachment>> {
        (**self).attachments(id)
    }

    fn read_attachment(
        &self,
        id: &NodeId,
        name: &str,
    ) -> BackendResult<Option<(Attachment, Vec<u8>)>> {
        (**self).read_attachment(id, name)
    }

    fn attach(
        &self,
        id: &NodeId,
        name: &str,
        bytes: &[u8],
        by: &Provenance,
    ) -> BackendResult<Attachment> {
        (**self).attach(id, name, bytes, by)
    }

    fn attach_file(&self, id: &NodeId, path: &Path, by: &Provenance) -> BackendResult<Attachment> {
        (**self).attach_file(id, path, by)
    }

    fn folders(&self) -> BackendResult<Vec<LocalFolder>> {
        (**self).folders()
    }
}
