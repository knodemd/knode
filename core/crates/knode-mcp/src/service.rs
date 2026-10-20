//! [`ServiceBackend`]: the tools read and write through Knode Service.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use knode_format::{
    Attachment, CanvasBoard, Change, ChangeDetail, ChangeId, HistoryEntry, Link, LinkKind,
    LinkKindSpec, NewNode, Node, NodeId, NodeLink, NodeSummary, NodeUpdate, Provenance, SearchMode,
    View,
};
use knode_service::{Client, Error};

use crate::backend::{Backend, BackendError, BackendResult, LocalFolder, SearchResults};

type Connect = dyn Fn() -> knode_service::Result<Client> + Send + Sync;

/// A backend that talks to Knode Service, the one process that opens spaces, so the agent's
/// writes and everyone else's go through one writer and the Knode app sees them as they happen.
///
/// When the service has stopped since the last call (it was restarted, or stopped and started
/// again), the next call connects again with the function given to [`ServiceBackend::new`].
/// A request whose answer was lost is not sent again, so a write is never made twice.
pub struct ServiceBackend {
    space: PathBuf,
    connect: Box<Connect>,
    client: Mutex<Option<Arc<Client>>>,
}

impl fmt::Debug for ServiceBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceBackend")
            .field("space", &self.space)
            .finish_non_exhaustive()
    }
}

impl ServiceBackend {
    /// A backend for the space at `space`, connected now with `connect`, which is also used to
    /// connect again later, for example `|| knode_service::connect_or_spawn(&exe)`. Fails when
    /// the service cannot be reached, so the caller can fall back to opening the space itself.
    pub fn new(
        space: impl Into<PathBuf>,
        connect: impl Fn() -> knode_service::Result<Client> + Send + Sync + 'static,
    ) -> knode_service::Result<Self> {
        let client = connect()?;
        Ok(ServiceBackend {
            space: space.into(),
            connect: Box::new(connect),
            client: Mutex::new(Some(Arc::new(client))),
        })
    }

    /// The space this backend reads and writes.
    pub fn path(&self) -> &Path {
        &self.space
    }

    /// The connection, made again if the service closed it.
    fn client(&self) -> BackendResult<Arc<Client>> {
        let mut guard: MutexGuard<'_, Option<Arc<Client>>> = self
            .client
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(client) = guard.as_ref().filter(|c| !c.is_closed()) {
            return Ok(client.clone());
        }
        *guard = None;
        let client = Arc::new((self.connect)().map_err(|e| self.error(e))?);
        *guard = Some(client.clone());
        Ok(client)
    }

    fn with<T>(&self, f: impl Fn(&Client, &Path) -> knode_service::Result<T>) -> BackendResult<T> {
        let client = self.client()?;
        match f(&client, &self.space) {
            // Not sent at all: the connection broke just now. Connect again and send it once.
            Err(Error::Io(_)) => {
                self.forget(&client);
                let client = self.client()?;
                f(&client, &self.space).map_err(|e| self.error(e))
            }
            Err(Error::Disconnected) => {
                self.forget(&client);
                Err(self.error(Error::Disconnected))
            }
            result => result.map_err(|e| self.error(e)),
        }
    }

    fn forget(&self, client: &Arc<Client>) {
        let mut guard = self
            .client
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.as_ref().is_some_and(|c| Arc::ptr_eq(c, client)) {
            *guard = None;
        }
    }

    fn error(&self, error: Error) -> BackendError {
        match error {
            Error::Rpc(rpc) => match rpc.kind() {
                Some(
                    "not_found" | "invalid" | "invalid_params" | "cannot_undo" | "already_undone"
                    | "undo_conflict",
                ) => BackendError::Invalid(rpc.message),
                // The service refused the agent's key: tell the person how to connect it again.
                Some("unauthenticated") => BackendError::Unavailable(format!(
                    "Knode refused this agent: {}. Ask the user to run `knode connect <agent>` \
                     in a terminal (for example `knode connect claude-code`).",
                    rpc.message.trim_end_matches('.')
                )),
                Some("forbidden") => BackendError::Invalid(rpc.message),
                Some("not_a_space" | "unsupported") => BackendError::Unavailable(format!(
                    "cannot open the Knode space at {}: {}",
                    self.space.display(),
                    rpc.message
                )),
                _ => BackendError::Failed(rpc.message),
            },
            Error::Disconnected => BackendError::Unavailable(
                "the connection to Knode Service closed before it answered; check whether the \
                 change was saved before trying again"
                    .to_string(),
            ),
            other => BackendError::Unavailable(format!(
                "Knode Service cannot be reached: {other}. Ask the user to run `knode service \
                 start` in a terminal."
            )),
        }
    }
}

impl Backend for ServiceBackend {
    fn search(&self, query: &str, mode: SearchMode, limit: usize) -> BackendResult<SearchResults> {
        let found = self.with(|c, s| c.search_with_mode(s, query, mode, limit))?;
        Ok(SearchResults {
            mode: found.mode,
            hits: found.hits,
            note: found.note,
        })
    }

    fn get_node(&self, id: &NodeId) -> BackendResult<Option<Node>> {
        self.with(|c, s| c.get_node(s, id))
    }

    fn find_by_title(&self, title: &str) -> BackendResult<Vec<NodeSummary>> {
        self.with(|c, s| c.find_by_title(s, title))
    }

    fn create_node(&self, new: NewNode, by: &Provenance) -> BackendResult<Node> {
        self.with(|c, s| c.create_node(s, new.clone(), by))
    }

    fn update_node(&self, id: &NodeId, update: NodeUpdate, by: &Provenance) -> BackendResult<Node> {
        self.with(|c, s| c.update_node(s, id, update.clone(), by))
    }

    fn link(&self, from: &NodeId, to: &NodeId, kind: &str, by: &Provenance) -> BackendResult<Link> {
        self.with(|c, s| c.link(s, from, to, kind, by))
    }

    fn links_from(&self, id: &NodeId) -> BackendResult<Vec<Link>> {
        self.with(|c, s| c.links_from(s, id))
    }

    fn backlinks(&self, id: &NodeId) -> BackendResult<Vec<Link>> {
        self.with(|c, s| c.backlinks(s, id))
    }

    fn recent(&self, limit: usize) -> BackendResult<Vec<NodeSummary>> {
        self.with(|c, s| c.recent(s, limit))
    }

    fn history(&self, id: &NodeId) -> BackendResult<Vec<HistoryEntry>> {
        self.with(|c, s| c.history(s, id))
    }

    fn changes(&self, limit: usize, before: Option<ChangeId>) -> BackendResult<Vec<Change>> {
        self.with(|c, s| c.changes(s, limit, before))
    }

    fn changes_of(&self, subject: &str, limit: usize) -> BackendResult<Vec<Change>> {
        self.with(|c, s| c.changes_of(s, subject, limit))
    }

    fn change(&self, id: ChangeId) -> BackendResult<Option<ChangeDetail>> {
        self.with(|c, s| c.change(s, id))
    }

    fn undo(&self, id: ChangeId, force: bool, by: &Provenance) -> BackendResult<Change> {
        self.with(|c, s| c.undo(s, id, force, by))
    }

    fn restore(&self, id: &NodeId, version: i64, by: &Provenance) -> BackendResult<Node> {
        self.with(|c, s| c.restore(s, id, version, by))
    }

    fn knows_the_agent(&self) -> bool {
        true
    }

    fn link_kinds(&self) -> BackendResult<Vec<LinkKind>> {
        self.with(|c, s| c.link_kinds(s))
    }

    fn set_link_kind(&self, spec: &LinkKindSpec, by: &Provenance) -> BackendResult<LinkKind> {
        self.with(|c, s| c.set_link_kind(s, spec, by))
    }

    fn remove_link_kind(&self, kind: &str, by: &Provenance) -> BackendResult<bool> {
        self.with(|c, s| c.remove_link_kind(s, kind, by))
    }

    fn node_links(&self, id: &NodeId) -> BackendResult<Vec<NodeLink>> {
        self.with(|c, s| c.node_links(s, id))
    }

    fn views(&self) -> BackendResult<Vec<View>> {
        self.with(|c, s| c.views(s))
    }

    fn canvas_board(&self, node: &NodeId) -> BackendResult<Option<CanvasBoard>> {
        self.with(|c, s| c.canvas_board(s, node))
    }

    fn attachments(&self, id: &NodeId) -> BackendResult<Vec<Attachment>> {
        self.with(|c, s| c.attachments(s, id))
    }

    fn read_attachment(
        &self,
        id: &NodeId,
        name: &str,
    ) -> BackendResult<Option<(Attachment, Vec<u8>)>> {
        self.with(|c, s| c.read_attachment(s, id, name))
    }

    fn attach(
        &self,
        id: &NodeId,
        name: &str,
        bytes: &[u8],
        by: &Provenance,
    ) -> BackendResult<Attachment> {
        self.with(|c, s| c.attach(s, id, name, bytes, by))
    }

    fn attach_file(&self, id: &NodeId, path: &Path, by: &Provenance) -> BackendResult<Attachment> {
        self.with(|c, s| c.attach_file(s, id, path, by))
    }

    fn folders(&self) -> BackendResult<Vec<LocalFolder>> {
        self.with(|c, s| {
            Ok(c.folders(s)?
                .into_iter()
                .map(|f| LocalFolder {
                    kind: f.folder.kind,
                    capture: f.folder.capture,
                    path: f.local,
                })
                .collect())
        })
    }
}
