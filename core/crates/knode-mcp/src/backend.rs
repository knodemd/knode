//! Where the MCP tools read and write: a [`Backend`].
//!
//! [`DirectBackend`] opens the space itself with `knode-format`.
//! [`ServiceBackend`](crate::ServiceBackend) talks to Knode Service over its socket and
//! implements the same trait, so the tools do not change.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use knode_format::{
    Link, NewNode, Node, NodeId, NodeSummary, NodeUpdate, Provenance, SearchHit, Space,
};

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
            knode_format::Error::Invalid(_) | knode_format::Error::NotFound(_) => {
                BackendError::Invalid(error.to_string())
            }
            other => BackendError::Failed(other.to_string()),
        }
    }
}

/// A `Result` whose error is [`BackendError`].
pub type BackendResult<T> = Result<T, BackendError>;

/// The operations the MCP tools need from a space. The methods mirror [`Space`] and the
/// Knode Service methods of the same name.
pub trait Backend: Send + Sync + 'static {
    /// `search`: nodes matching the words, best first.
    fn search(&self, query: &str, limit: usize) -> BackendResult<Vec<SearchHit>>;
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
}

/// A backend that opens the space directly with `knode-format`.
///
/// The space is opened on first use and kept open. If it cannot be opened, each call says why
/// and the next call tries again, so the server keeps running while the person fixes it.
#[derive(Debug)]
pub struct DirectBackend {
    target: Target,
    space: Mutex<Option<Space>>,
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
        }
    }

    /// A backend with no space, whose every call fails with `message`, such as how to create
    /// a space. The server still starts, so the agent can tell the person what to do.
    pub fn unavailable(message: impl Into<String>) -> Self {
        DirectBackend {
            target: Target::Missing(message.into()),
            space: Mutex::new(None),
        }
    }

    /// A backend for a space that is already open.
    pub fn from_space(space: Space) -> Self {
        DirectBackend {
            target: Target::Path(space.path().to_path_buf()),
            space: Mutex::new(Some(space)),
        }
    }

    /// The space this backend opens, if it has one.
    pub fn path(&self) -> Option<&Path> {
        match &self.target {
            Target::Path(path) => Some(path),
            Target::Missing(_) => None,
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut Space) -> knode_format::Result<T>) -> BackendResult<T> {
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
        Ok(f(space)?)
    }
}

impl Backend for DirectBackend {
    fn search(&self, query: &str, limit: usize) -> BackendResult<Vec<SearchHit>> {
        self.with(|s| s.search(query, limit))
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
}
