//! The wire protocol: JSON-RPC 2.0 over a Unix socket, one JSON message per line.
//!
//! See `PROTOCOL.md` in this crate for the full description.

use std::fmt;
use std::path::PathBuf;

use knode_format::{ChangeId, NodeId, NodeUpdate, Provenance, SearchMode, Space};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::search::IndexStatus;
use crate::spaces::{FolderInput, SpaceEvent};

/// The method names.
pub mod methods {
    /// Creates a space.
    pub const SPACE_CREATE: &str = "space.create";
    /// Opens a space.
    pub const SPACE_OPEN: &str = "space.open";
    /// Renames a space. People only.
    pub const SPACE_RENAME: &str = "space.rename";
    /// The settings of a space.
    pub const SETTINGS_GET: &str = "settings.get";
    /// Changes a setting of a space. People only.
    pub const SETTINGS_SET: &str = "settings.set";
    /// The folders that feed a space.
    pub const FOLDERS_LIST: &str = "folders.list";
    /// Adds or changes a folder of a space. People only.
    pub const FOLDERS_SET: &str = "folders.set";
    /// Removes a folder of a space. People only.
    pub const FOLDERS_REMOVE: &str = "folders.remove";
    /// Creates a node.
    pub const NODE_CREATE: &str = "node.create";
    /// Reads a node.
    pub const NODE_GET: &str = "node.get";
    /// Finds the nodes with a title.
    pub const NODE_FIND_BY_TITLE: &str = "node.find_by_title";
    /// Changes a node.
    pub const NODE_UPDATE: &str = "node.update";
    /// Deletes a node.
    pub const NODE_DELETE: &str = "node.delete";
    /// Links two nodes.
    pub const LINK_CREATE: &str = "link.create";
    /// Removes a link.
    pub const LINK_DELETE: &str = "link.delete";
    /// The links from a node.
    pub const LINKS_FROM: &str = "links.from";
    /// The links to a node.
    pub const LINKS_BACK: &str = "links.back";
    /// The `[[links]]` of a node that name no node yet.
    pub const LINKS_UNRESOLVED: &str = "links.unresolved";
    /// Search by words, by meaning or both.
    pub const SEARCH: &str = "search";
    /// How far the open spaces are indexed for search by meaning. People only.
    pub const INDEX_STATUS: &str = "index.status";
    /// Indexes the open spaces for search by meaning now. People only.
    pub const INDEX_RUN: &str = "index.run";
    /// The nodes changed most recently.
    pub const RECENT: &str = "recent";
    /// Every version of a node.
    pub const HISTORY: &str = "history";
    /// The changes of a space, or of one subject such as a node, newest first.
    pub const CHANGES: &str = "changes";
    /// One change with the versions and links it wrote.
    pub const CHANGE: &str = "change";
    /// Undoes a change.
    pub const UNDO: &str = "undo";
    /// Puts a node back to one of its versions, or recovers a deleted node.
    pub const RESTORE: &str = "restore";
    /// Erases a node and every version of it, for good. People only.
    pub const PURGE: &str = "purge";
    /// Writes a space to a folder of Markdown files.
    pub const VAULT_EXPORT: &str = "vault.export";
    /// Reads a folder of Markdown files, or an Obsidian vault, into a space.
    pub const VAULT_IMPORT: &str = "vault.import";
    /// The live Markdown folder of a space.
    pub const MARKDOWN_FOLDER_GET: &str = "markdown.folder.get";
    /// Turns the live Markdown folder of a space on, moves it, or turns it off. People only.
    pub const MARKDOWN_FOLDER_SET: &str = "markdown.folder.set";
    /// Starts `changed` notifications on this connection.
    pub const SUBSCRIBE: &str = "subscribe";
    /// Stops `changed` notifications on this connection.
    pub const UNSUBSCRIBE: &str = "unsubscribe";
    /// What the service is doing.
    pub const SERVICE_STATUS: &str = "service.status";
    /// Authenticates the connection, as the person or as an agent.
    pub const AUTH: &str = "auth";
    /// The agents of this machine. People only.
    pub const AGENTS_LIST: &str = "agents.list";
    /// Adds an agent, or gives it a new key. People only.
    pub const AGENTS_ADD: &str = "agents.add";
    /// Revokes every key of an agent. People only.
    pub const AGENTS_REVOKE: &str = "agents.revoke";
    /// Sets what an agent may do in a space. People only.
    pub const GRANTS_SET: &str = "grants.set";
    /// Notification: a node changed.
    pub const CHANGED: &str = "changed";
    /// Notification: a space was renamed, or its settings or folders changed.
    pub const SPACE: &str = "space";
    /// Notification: this connection fell behind and missed changes.
    pub const LAGGED: &str = "lagged";
}

/// The JSON-RPC error codes the service answers with.
pub mod codes {
    /// The line is not valid JSON.
    pub const PARSE_ERROR: i64 = -32700;
    /// The message is not a valid JSON-RPC 2.0 request.
    pub const INVALID_REQUEST: i64 = -32600;
    /// No method has this name.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// The params are missing, of the wrong type or out of range.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Something went wrong inside the service.
    pub const INTERNAL_ERROR: i64 = -32603;
    /// No node has this id.
    pub const NOT_FOUND: i64 = -32001;
    /// The path is not a `.knode` package.
    pub const NOT_A_SPACE: i64 = -32002;
    /// A space already exists at this path.
    pub const ALREADY_EXISTS: i64 = -32003;
    /// A value breaks a rule of the format, such as an empty title.
    pub const INVALID: i64 = -32004;
    /// The space was written by a newer version of Knode, or its files do not agree.
    pub const UNSUPPORTED: i64 = -32005;
    /// The files of the space could not be read or written.
    pub const STORAGE: i64 = -32006;
    /// The connection has not authenticated, or its key is unknown or revoked.
    pub const UNAUTHENTICATED: i64 = -32010;
    /// The connection may not make this call: an agent writing where it may only read, or
    /// doing what only a person can.
    pub const FORBIDDEN: i64 = -32011;
    /// The change cannot be undone: its kind of change is not undone, or the versions it
    /// needs were purged.
    pub const CANNOT_UNDO: i64 = -32020;
    /// The change was already undone.
    pub const ALREADY_UNDONE: i64 = -32021;
    /// Undoing the change would overwrite later changes; `data.later` lists them.
    pub const UNDO_CONFLICT: i64 = -32022;
    /// A folder of Markdown files could not be read or written.
    pub const FOLDER: i64 = -32024;
}

/// An error answered by the service: a JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    /// The error code, one of [`codes`].
    pub code: i64,
    /// What went wrong, for people.
    pub message: String,
    /// More detail: `{"kind": "..."}` with a stable name for the error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// An error with a code and a message.
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        RpcError {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub(crate) fn with_kind(mut self, kind: &str) -> Self {
        self.data = Some(serde_json::json!({ "kind": kind }));
        self
    }

    /// The stable name of the error from `data.kind`, such as `not_found`.
    pub fn kind(&self) -> Option<&str> {
        self.data.as_ref()?.get("kind")?.as_str()
    }

    pub(crate) fn invalid_params(message: impl Into<String>) -> Self {
        RpcError::new(codes::INVALID_PARAMS, message).with_kind("invalid_params")
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

impl From<knode_vault::Error> for RpcError {
    fn from(e: knode_vault::Error) -> Self {
        match e {
            knode_vault::Error::Space(e) => e.into(),
            other => RpcError::new(codes::FOLDER, other.to_string()).with_kind("folder"),
        }
    }
}

impl From<knode_format::Error> for RpcError {
    fn from(e: knode_format::Error) -> Self {
        use knode_format::Error as E;
        let message = e.to_string();
        let (code, kind) = match e {
            E::NotFound(_) | E::ChangeNotFound(_) => (codes::NOT_FOUND, "not_found"),
            E::CannotUndo { .. } => (codes::CANNOT_UNDO, "cannot_undo"),
            E::AlreadyUndone { undone_by, .. } => {
                let mut error = RpcError::new(codes::ALREADY_UNDONE, message);
                error.data = Some(serde_json::json!({
                    "kind": "already_undone",
                    "undone_by": undone_by,
                }));
                return error;
            }
            E::UndoConflict { later, .. } => {
                let mut error = RpcError::new(codes::UNDO_CONFLICT, message);
                error.data = Some(serde_json::json!({ "kind": "undo_conflict", "later": later }));
                return error;
            }
            E::NotASpace(_) => (codes::NOT_A_SPACE, "not_a_space"),
            E::AlreadyExists(_) => (codes::ALREADY_EXISTS, "already_exists"),
            E::Invalid(_) => (codes::INVALID, "invalid"),
            E::UnsupportedFormat { .. } | E::NewerSchema { .. } | E::MismatchedSpace { .. } => {
                (codes::UNSUPPORTED, "unsupported")
            }
            E::Io(_) | E::Sqlite(_) | E::Manifest(_) => (codes::STORAGE, "storage"),
            E::PersonOnly(_) => (codes::FORBIDDEN, "forbidden"),
            _ => (codes::INTERNAL_ERROR, "internal"),
        };
        RpcError::new(code, message).with_kind(kind)
    }
}

/// What happened to a node, in a [`Change`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    /// The node was created.
    Create,
    /// The node was changed, directly or because a node it links to was renamed.
    Update,
    /// The node was deleted.
    Delete,
    /// A link from the node was made.
    Link,
    /// A link from the node was removed.
    Unlink,
}

/// The params of a `changed` notification: one node changed in one space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// The space, as the service opened it (an absolute, canonical path).
    pub space: PathBuf,
    /// The node that changed.
    pub node_id: NodeId,
    /// What happened to it.
    pub op: ChangeKind,
}

/// A notification received by a subscribed [`Client`](crate::Client).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A node changed.
    Changed(Change),
    /// A space was renamed, or its settings or folders changed.
    Space(SpaceEvent),
    /// The connection fell behind and missed this many changes; reload what is shown.
    Lagged {
        /// How many changes were missed.
        missed: u64,
    },
}

/// A space as the service has it open: the result of `space.create` and `space.open`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceInfo {
    /// The package folder, absolute and canonical. Use it as `space` in later calls.
    pub path: PathBuf,
    /// The id of the space from `manifest.json`.
    pub id: String,
    /// The name of the space.
    pub name: String,
    /// The format version.
    pub format: u64,
    /// When the space was created, in RFC 3339.
    pub created_at: String,
    /// How many nodes it holds.
    pub nodes: u64,
}

impl SpaceInfo {
    pub(crate) fn of(space: &Space) -> knode_format::Result<Self> {
        let manifest = space.manifest();
        Ok(SpaceInfo {
            path: space.path().to_path_buf(),
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            format: manifest.format,
            created_at: manifest.created_at.clone(),
            nodes: space.node_count()?,
        })
    }
}

/// What the service is doing: the result of `service.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// The version of Knode Service.
    pub version: String,
    /// The process id of the service.
    pub pid: u32,
    /// The spaces the service has open.
    pub spaces: Vec<PathBuf>,
    /// How many clients are connected, this one included.
    pub clients: u64,
    /// The default space while capture runs, where conversations of no folder go, or `None`.
    #[serde(default)]
    pub capture: Option<PathBuf>,
    /// Search by meaning: the model and how far each open space is indexed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<IndexStatus>,
}

// Params of each method, shared by the client and the server.

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SpaceParams {
    pub space: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SpaceRenameParams {
    pub space: PathBuf,
    pub name: String,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SettingsSetParams {
    pub space: PathBuf,
    pub key: String,
    pub value: Option<String>,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct FolderSetParams {
    pub space: PathBuf,
    #[serde(flatten)]
    pub folder: FolderInput,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct FolderRemoveParams {
    pub space: PathBuf,
    pub id: String,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SpaceCreateParams {
    pub space: PathBuf,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct NodeCreateParams {
    pub space: PathBuf,
    #[serde(default)]
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct NodeParams {
    pub space: PathBuf,
    pub id: NodeId,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TitleParams {
    pub space: PathBuf,
    pub title: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct NodeUpdateParams {
    pub space: PathBuf,
    pub id: NodeId,
    #[serde(flatten)]
    pub update: NodeUpdate,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct NodeDeleteParams {
    pub space: PathBuf,
    pub id: NodeId,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LinkCreateParams {
    pub space: PathBuf,
    pub from: NodeId,
    pub to: NodeId,
    #[serde(default = "default_link_kind")]
    pub kind: String,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LinkDeleteParams {
    pub space: PathBuf,
    pub from: NodeId,
    pub to: NodeId,
    #[serde(default = "default_link_kind")]
    pub kind: String,
    /// Left out by clients older than v0.2: the change is then recorded as Knode's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<Provenance>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChangesParams {
    pub space: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<ChangeId>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChangeParams {
    pub space: PathBuf,
    pub id: ChangeId,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct UndoParams {
    pub space: PathBuf,
    pub change: ChangeId,
    #[serde(default)]
    pub force: bool,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct RestoreParams {
    pub space: PathBuf,
    pub id: NodeId,
    pub version: i64,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct VaultExportParams {
    pub space: PathBuf,
    pub dir: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct VaultImportParams {
    pub space: PathBuf,
    pub dir: PathBuf,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct MarkdownFolderSetParams {
    pub space: PathBuf,
    pub folder: Option<PathBuf>,
    pub by: Provenance,
}

/// The live Markdown folder of a space: the result of `markdown.folder.get` and
/// `markdown.folder.set`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MarkdownFolder {
    /// The folder as the space records it (`~/...` or absolute), or `None` when it is off.
    pub folder: Option<String>,
    /// The folder on this machine, or `None` when it is off.
    pub path: Option<PathBuf>,
    /// A warning for a folder inside iCloud Drive, Dropbox, OneDrive or Google Drive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SearchParams {
    pub space: PathBuf,
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<SearchMode>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct RecentParams {
    pub space: PathBuf,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct SubscribeParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LaggedParams {
    pub missed: u64,
}

fn default_link_kind() -> String {
    knode_format::link_kinds::RELATED.to_string()
}

/// The `limit` of `search` and `recent` when none is given.
pub const DEFAULT_LIMIT: usize = 20;
/// The largest `limit` the service accepts.
pub const MAX_LIMIT: usize = 1000;
/// The longest line, in bytes, the service reads. Longer lines close the connection.
pub const MAX_LINE: usize = 16 * 1024 * 1024;

fn default_limit() -> usize {
    DEFAULT_LIMIT
}
