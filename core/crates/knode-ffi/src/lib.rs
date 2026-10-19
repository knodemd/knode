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
