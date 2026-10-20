//! The bridge to Swift, Kotlin and the web, generated with UniFFI.
//!
//! [`KnodeSpace`] wraps a [`knode_format::Space`] behind a mutex, so the apps can call it from any
//! thread. Ids are ULID strings and dates are milliseconds since the Unix epoch, in UTC.
//!
//! The Swift bindings are generated from the built library with the `uniffi-bindgen` binary of
//! this crate (`--features bindgen`); `mac/scripts/build-xcframework.sh` does it for the Mac app.

#![warn(missing_docs)]

use std::sync::{Arc, Mutex, MutexGuard};

use knode_format as format;
use knode_format::NodeId;

mod maps;
mod spaces;

pub use maps::*;
pub use spaces::*;

uniffi::setup_scaffolding!();

/// The version of this crate.
#[uniffi::export]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Everything that can go wrong reading or writing a space.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum KnodeError {
    /// The path is not a `.knode` package.
    #[error("{path} is not a .knode package")]
    NotASpace {
        /// The path that was opened.
        path: String,
    },
    /// A space cannot be created where one already exists.
    #[error("{path} already exists")]
    AlreadyExists {
        /// The path that was given.
        path: String,
    },
    /// The space was written by a newer version of Knode, or its files do not agree.
    #[error("{message}")]
    Unsupported {
        /// What is wrong.
        message: String,
    },
    /// No node has this id.
    #[error("node {id} not found")]
    NotFound {
        /// The id that was asked for.
        id: String,
    },
    /// An argument broke a rule of the format, such as an empty title or a bad id.
    #[error("{message}")]
    Invalid {
        /// What is wrong.
        message: String,
    },
    /// A file or the database could not be read or written.
    #[error("{message}")]
    Storage {
        /// What went wrong.
        message: String,
    },
    /// No change has this id.
    #[error("change {id} not found")]
    ChangeNotFound {
        /// The id that was asked for.
        id: i64,
    },
    /// The change cannot be undone: its action cannot be undone, the versions it needs were
    /// purged, or it was already undone.
    #[error("{message}")]
    CannotUndo {
        /// Why not, written for people.
        message: String,
    },
    /// Later changes touched what the change touched: undoing it would overwrite them. Ask the
    /// person before undoing it again with `force`.
    #[error("{message}")]
    UndoConflict {
        /// The change.
        change: i64,
        /// The later changes, oldest first.
        later: Vec<i64>,
        /// What would be overwritten, written for people.
        message: String,
    },
    /// Only a person may do this, never an agent.
    #[error("{message}")]
    PersonOnly {
        /// What was refused, written for people.
        message: String,
    },
    /// The Knode settings (`config.toml`) could not be found, read or written.
    #[error("{message}")]
    Config {
        /// What went wrong.
        message: String,
    },
}

impl From<format::Error> for KnodeError {
    fn from(error: format::Error) -> Self {
        use format::Error as E;
        match error {
            E::NotASpace(path) => KnodeError::NotASpace {
                path: path.display().to_string(),
            },
            E::AlreadyExists(path) => KnodeError::AlreadyExists {
                path: path.display().to_string(),
            },
            E::NotFound(id) => KnodeError::NotFound { id: id.to_string() },
            E::Invalid(message) => KnodeError::Invalid { message },
            E::ChangeNotFound(id) => KnodeError::ChangeNotFound { id },
            e @ (E::CannotUndo { .. } | E::AlreadyUndone { .. }) => KnodeError::CannotUndo {
                message: e.to_string(),
            },
            E::UndoConflict { change, later } => {
                let message = E::UndoConflict {
                    change,
                    later: later.clone(),
                }
                .to_string();
                KnodeError::UndoConflict {
                    change,
                    later,
                    message,
                }
            }
            e @ E::PersonOnly(_) => KnodeError::PersonOnly {
                message: e.to_string(),
            },
            e @ (E::UnsupportedFormat { .. }
            | E::NewerSchema { .. }
            | E::MismatchedSpace { .. }) => KnodeError::Unsupported {
                message: e.to_string(),
            },
            e => KnodeError::Storage {
                message: e.to_string(),
            },
        }
    }
}

type Result<T, E = KnodeError> = std::result::Result<T, E>;

/// Who made a change: a person, an agent or Knode itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ActorKind {
    /// A person, such as the owner of the space.
    Person,
    /// An AI agent, such as `claude-code` or `codex`.
    Agent,
    /// Knode itself.
    System,
}

/// Who or what made a change, and from where.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Provenance {
    /// A person, an agent or Knode itself.
    pub kind: ActorKind,
    /// The name of the person or agent.
    pub actor: String,
    /// The conversation or session the change came from.
    pub session: Option<String>,
    /// Where the content came from: a file path, a URL, a project folder.
    pub source: Option<String>,
}

impl From<format::Provenance> for Provenance {
    fn from(p: format::Provenance) -> Self {
        Provenance {
            kind: match p.kind {
                format::ActorKind::Person => ActorKind::Person,
                format::ActorKind::Agent => ActorKind::Agent,
                format::ActorKind::System => ActorKind::System,
            },
            actor: p.actor,
            session: p.session,
            source: p.source,
        }
    }
}

impl From<Provenance> for format::Provenance {
    fn from(p: Provenance) -> Self {
        let kind = match p.kind {
            ActorKind::Person => format::ActorKind::Person,
            ActorKind::Agent => format::ActorKind::Agent,
            ActorKind::System => format::ActorKind::System,
        };
        format::Provenance {
            kind,
            actor: p.actor,
            session: p.session,
            source: p.source,
        }
    }
}

/// What a space says about itself in `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SpaceInfo {
    /// The space id, a ULID.
    pub id: String,
    /// The space name, shown to people.
    pub name: String,
    /// The package folder.
    pub path: String,
}

/// A node with its whole Markdown body.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Node {
    /// The node id, a ULID.
    pub id: String,
    /// What the node is, such as `note` or `conversation`.
    pub kind: String,
    /// The title.
    pub title: String,
    /// The text, in Markdown.
    pub body: String,
    /// Tags, lower case, sorted.
    pub tags: Vec<String>,
    /// When the node was created, in Unix milliseconds.
    pub created_at: i64,
    /// When the node last changed, in Unix milliseconds.
    pub updated_at: i64,
    /// Who created the node.
    pub created_by: Provenance,
    /// Who changed the node last.
    pub updated_by: Provenance,
}

impl From<format::Node> for Node {
    fn from(n: format::Node) -> Self {
        Node {
            id: n.id.to_string(),
            kind: n.kind,
            title: n.title,
            body: n.body,
            tags: n.tags,
            created_at: n.created_at.as_millis(),
            updated_at: n.updated_at.as_millis(),
            created_by: n.created_by.into(),
            updated_by: n.updated_by.into(),
        }
    }
}

/// A node without its body, for lists.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NodeSummary {
    /// The node id, a ULID.
    pub id: String,
    /// What the node is.
    pub kind: String,
    /// The title.
    pub title: String,
    /// When the node was created, in Unix milliseconds.
    pub created_at: i64,
    /// When the node last changed, in Unix milliseconds.
    pub updated_at: i64,
    /// Who changed the node last.
    pub updated_by: Provenance,
}

impl From<format::NodeSummary> for NodeSummary {
    fn from(n: format::NodeSummary) -> Self {
        NodeSummary {
            id: n.id.to_string(),
            kind: n.kind,
            title: n.title,
            created_at: n.created_at.as_millis(),
            updated_at: n.updated_at.as_millis(),
            updated_by: n.updated_by.into(),
        }
    }
}

/// A node in a list, with the first line of its text.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RecentNode {
    /// The node.
    pub node: NodeSummary,
    /// The first line of plain text in the body, without heading, quote or list markers.
    pub excerpt: String,
}

/// A node found by a search.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SearchHit {
    /// The node.
    pub node: NodeSummary,
    /// A short piece of the text around the words found, each match in `**bold**`, `…` where
    /// text was cut.
    pub snippet: String,
    /// How well the node matches; higher is better. Only comparable within one search.
    pub score: f64,
}

impl From<format::SearchHit> for SearchHit {
    fn from(h: format::SearchHit) -> Self {
        SearchHit {
            node: h.node.into(),
            snippet: h.snippet,
            score: h.score,
        }
    }
}

/// A typed link from one node to another.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Link {
    /// The node the link starts from.
    pub from: String,
    /// The title of the node the link starts from.
    pub from_title: String,
    /// The node the link points at.
    pub to: String,
    /// The title of the node the link points at.
    pub to_title: String,
    /// The type of link, such as `mentions` or `related`.
    pub kind: String,
    /// When the link was made, in Unix milliseconds.
    pub created_at: i64,
    /// Who made the link.
    pub created_by: Provenance,
}

impl From<format::Link> for Link {
    fn from(l: format::Link) -> Self {
        Link {
            from: l.from.to_string(),
            from_title: l.from_title,
            to: l.to.to_string(),
            to_title: l.to_title,
            kind: l.kind,
            created_at: l.created_at.as_millis(),
            created_by: l.created_by.into(),
        }
    }
}

/// What a version in the history of a node did to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ChangeOp {
    /// The node was created.
    Create,
    /// The node was changed.
    Update,
    /// The node was deleted.
    Delete,
}

impl From<format::ChangeOp> for ChangeOp {
    fn from(op: format::ChangeOp) -> Self {
        match op {
            format::ChangeOp::Create => ChangeOp::Create,
            format::ChangeOp::Update => ChangeOp::Update,
            format::ChangeOp::Delete => ChangeOp::Delete,
        }
    }
}

/// One version of a node: the node as it was right after a change (for a delete, right before
/// it), and who made the change and when.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct HistoryEntry {
    /// The order of the version in the space; it only grows.
    pub seq: i64,
    /// The node.
    pub node_id: String,
    /// What the change did to the node.
    pub op: ChangeOp,
    /// When the change was made, in Unix milliseconds.
    pub changed_at: i64,
    /// Who made the change.
    pub changed_by: Provenance,
    /// What the node was.
    pub kind: String,
    /// The title.
    pub title: String,
    /// The text, in Markdown.
    pub body: String,
    /// The tags.
    pub tags: Vec<String>,
    /// The change the version belongs to, which [`KnodeSpace::undo`] takes. `None` only in
    /// spaces written before schema version 3 that were never migrated.
    pub change_id: Option<i64>,
    /// When the version puts back an earlier one (an undo or a restore), the `seq` of that one.
    pub restored_from: Option<i64>,
}

impl From<format::HistoryEntry> for HistoryEntry {
    fn from(h: format::HistoryEntry) -> Self {
        HistoryEntry {
            seq: h.seq,
            node_id: h.node_id.to_string(),
            op: h.op.into(),
            changed_at: h.changed_at.as_millis(),
            changed_by: h.changed_by.into(),
            kind: h.kind,
            title: h.title,
            body: h.body,
            tags: h.tags,
            change_id: h.change_id,
            restored_from: h.restored_from,
        }
    }
}

/// One change: one write that a person or an agent asked for.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Change {
    /// The order of the change in the space; it only grows.
    pub id: i64,
    /// When the change was made, in Unix milliseconds.
    pub changed_at: i64,
    /// Who made the change.
    pub changed_by: Provenance,
    /// What was done, such as `node.update`, `node.restore` or `undo`.
    pub action: String,
    /// What it was done to: a node id, a setting key, or empty.
    pub subject: String,
    /// For an `undo`, the change it undoes.
    pub undoes: Option<i64>,
}

impl From<format::Change> for Change {
    fn from(c: format::Change) -> Self {
        Change {
            id: c.id,
            changed_at: c.changed_at.as_millis(),
            changed_by: c.changed_by.into(),
            action: c.action,
            subject: c.subject,
            undoes: c.undoes,
        }
    }
}

/// An open `.knode` space. Safe to share between threads: calls take turns.
#[derive(uniffi::Object)]
pub struct KnodeSpace {
    space: Mutex<format::Space>,
}

#[uniffi::export]
impl KnodeSpace {
    /// Creates a new, empty space at `path`, which must end in `.knode` and must not exist.
    #[uniffi::constructor]
    pub fn create(path: String, name: String) -> Result<Arc<Self>> {
        Ok(Self::wrap(format::Space::create(path, &name)?))
    }

    /// Opens an existing space.
    #[uniffi::constructor]
    pub fn open(path: String) -> Result<Arc<Self>> {
        Ok(Self::wrap(format::Space::open(path)?))
    }

    /// The id, name and path of the space.
    pub fn info(&self) -> Result<SpaceInfo> {
        let space = self.lock();
        Ok(SpaceInfo {
            id: space.manifest().id.clone(),
            name: space.manifest().name.clone(),
            path: space.path().display().to_string(),
        })
    }

    /// The nodes changed most recently, newest first, at most `limit`.
    pub fn recent(&self, limit: u32) -> Result<Vec<RecentNode>> {
        let space = self.lock();
        let summaries = space.recent(limit as usize)?;
        summaries
            .into_iter()
            .map(|summary| {
                let excerpt = space
                    .get_node(&summary.id)?
                    .map(|node| excerpt(&node.body))
                    .unwrap_or_default();
                Ok(RecentNode {
                    node: summary.into(),
                    excerpt,
                })
            })
            .collect()
    }

    /// One node with its body, or `None` if no node has this id.
    pub fn get_node(&self, id: String) -> Result<Option<Node>> {
        let id = parse_id(&id)?;
        Ok(self.lock().get_node(&id)?.map(Node::from))
    }

    /// Creates a note and returns it. `[[links]]` in the body become links.
    pub fn create_node(&self, title: String, body: String, by: Provenance) -> Result<Node> {
        let node = self
            .lock()
            .create_node(format::NewNode::note(title, body), &by.into())?;
        Ok(node.into())
    }

    /// Changes the title, the body or both; `None` leaves a field as it is. A new title rewrites
    /// the `[[links]]` to the node.
    pub fn update_node(
        &self,
        id: String,
        title: Option<String>,
        body: Option<String>,
        by: Provenance,
    ) -> Result<Node> {
        let id = parse_id(&id)?;
        let update = format::NodeUpdate {
            title,
            body,
            ..Default::default()
        };
        Ok(self.lock().update_node(&id, update, &by.into())?.into())
    }

    /// Deletes a node and its links. Its last version stays in the history.
    pub fn delete_node(&self, id: String, by: Provenance) -> Result<()> {
        let id = parse_id(&id)?;
        Ok(self.lock().delete_node(&id, &by.into())?)
    }

    /// Searches titles and text by words, best match first, at most `limit` hits.
    pub fn search(&self, query: String, limit: u32) -> Result<Vec<SearchHit>> {
        let hits = self.lock().search(&query, limit as usize)?;
        Ok(hits.into_iter().map(SearchHit::from).collect())
    }

    /// The links that start from a node.
    pub fn links_from(&self, id: String) -> Result<Vec<Link>> {
        let id = parse_id(&id)?;
        let links = self.lock().links_from(&id)?;
        Ok(links.into_iter().map(Link::from).collect())
    }

    /// The links that point at a node.
    pub fn backlinks(&self, id: String) -> Result<Vec<Link>> {
        let id = parse_id(&id)?;
        let links = self.lock().backlinks(&id)?;
        Ok(links.into_iter().map(Link::from).collect())
    }

    /// Renames the space: changes `name` in `manifest.json`. The package folder keeps its name.
    pub fn rename(&self, name: String) -> Result<SpaceInfo> {
        self.lock().rename(&name)?;
        self.info()
    }

    /// A setting of the space, such as `space.icon`, or `None` when it is not set.
    pub fn setting(&self, key: String) -> Result<Option<String>> {
        Ok(self.lock().setting(&key)?)
    }

    /// Sets a setting of the space, or removes it with `None`, and records the change. Returns
    /// whether anything changed. Only a person can change settings.
    pub fn set_setting(&self, key: String, value: Option<String>, by: Provenance) -> Result<bool> {
        Ok(self
            .lock()
            .set_setting(&key, value.as_deref(), &by.into())?)
    }

    /// Every version of a node, oldest first, including its deletion. Works for deleted nodes.
    pub fn history(&self, id: String) -> Result<Vec<HistoryEntry>> {
        let id = parse_id(&id)?;
        let history = self.lock().history(&id)?;
        Ok(history.into_iter().map(HistoryEntry::from).collect())
    }

    /// The changes made to a node, newest first, at most `limit`.
    pub fn changes_of(&self, id: String, limit: u32) -> Result<Vec<Change>> {
        let id = parse_id(&id)?;
        let changes = self.lock().changes_of(&id.to_string(), limit as usize)?;
        Ok(changes.into_iter().map(Change::from).collect())
    }

    /// Puts a node back to one of its versions (`seq` of an entry of its history) and returns
    /// it. A deleted node comes back with the same id. Restoring the version the node already
    /// has writes nothing.
    pub fn restore(&self, id: String, seq: i64, by: Provenance) -> Result<Node> {
        let id = parse_id(&id)?;
        Ok(self.lock().restore(&id, seq, &by.into())?.into())
    }

    /// Undoes a change and returns the change that undoes it. Fails with
    /// [`KnodeError::UndoConflict`] when later changes touched the same nodes, unless `force`
    /// is set: confirm with the person first.
    pub fn undo(&self, change: i64, by: Provenance, force: bool) -> Result<Change> {
        Ok(self.lock().undo(change, &by.into(), force)?.into())
    }
}

impl KnodeSpace {
    fn wrap(space: format::Space) -> Arc<Self> {
        Arc::new(KnodeSpace {
            space: Mutex::new(space),
        })
    }

    fn lock(&self) -> MutexGuard<'_, format::Space> {
        // A panic while holding the lock cannot leave the database half written (every write is
        // a transaction), so a poisoned lock is still safe to use.
        self.space.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub(crate) fn config_error(error: knode_config::Error) -> KnodeError {
    KnodeError::Config {
        message: error.to_string(),
    }
}

pub(crate) fn config_paths() -> Result<knode_config::Paths> {
    knode_config::Paths::from_env().map_err(config_error)
}

/// The default space for agents, capture and commands: `default_space` in
/// `$KNODE_HOME/config.toml`, or `None` when there is none.
#[uniffi::export]
pub fn default_space() -> Result<Option<String>> {
    default_space_in(&config_paths()?)
}

/// Sets `default_space` in `$KNODE_HOME/config.toml`, or removes it with `None`. Every other
/// setting in the file is kept.
#[uniffi::export]
pub fn set_default_space(path: Option<String>) -> Result<()> {
    set_default_space_in(&config_paths()?, path)
}

/// The folder where new spaces go by default, `~/Knode`.
#[uniffi::export]
pub fn spaces_folder() -> Result<String> {
    Ok(config_paths()?.spaces_dir.display().to_string())
}

/// The `.knode` packages in the default spaces folder, sorted by path.
#[uniffi::export]
pub fn spaces_in_folder() -> Result<Vec<String>> {
    spaces_in_folder_of(&config_paths()?)
}

fn default_space_in(paths: &knode_config::Paths) -> Result<Option<String>> {
    let config = knode_config::Config::load(paths).map_err(config_error)?;
    Ok(config
        .default_space
        .filter(|path| !path.as_os_str().is_empty())
        .map(|path| path.display().to_string()))
}

fn set_default_space_in(paths: &knode_config::Paths, path: Option<String>) -> Result<()> {
    let mut config = knode_config::Config::load(paths).map_err(config_error)?;
    match path.filter(|p| !p.is_empty()) {
        // The previous default stays in `spaces`, so capture still routes over its folders.
        Some(path) => config.set_default_space(std::path::Path::new(&path)),
        None => {
            if let Some(previous) = config.default_space.take() {
                config.remember_space(&previous);
            }
        }
    }
    config.save(paths).map_err(config_error)
}

fn spaces_in_folder_of(paths: &knode_config::Paths) -> Result<Vec<String>> {
    let spaces = paths.spaces_in_dir().map_err(config_error)?;
    Ok(spaces.iter().map(|p| p.display().to_string()).collect())
}

fn parse_id(id: &str) -> Result<NodeId> {
    Ok(NodeId::parse(id)?)
}

/// The first line of plain text in a Markdown body, without heading, quote, list or task
/// markers, emphasis or table pipes, at most 140 characters.
fn excerpt(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("```"))
        // Table delimiter rows, such as `|---|:--|`.
        .filter(|line| !line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')))
        .map(|line| {
            let line = line.trim_start_matches(['#', '>', '-', '*', '+', ' ']);
            let line = ["[ ] ", "[x] ", "[X] "]
                .iter()
                .find_map(|task| line.strip_prefix(task))
                .unwrap_or(line);
            let line = line.replace("**", "").replace("__", "").replace('`', "");
            let cells: Vec<&str> = line
                .split('|')
                .map(str::trim)
                .filter(|cell| !cell.is_empty())
                .collect();
            cells.join("  ")
        })
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(140).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
