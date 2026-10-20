//! Read and write `.knode` spaces: nodes, links, provenance, history, search, views and
//! attachments.
//!
//! A space is a `.knode` package, a folder the Finder shows as one file:
//!
//! ```text
//! Example.knode/
//!   manifest.json   format version, id and name
//!   knode.db        SQLite in WAL mode: nodes, properties, links, provenance, history, search,
//!                   link kinds, views and attachments
//!   blobs/          attachments, named by the SHA-256 of their bytes
//!   sync/           Loro documents of node bodies (reserved for now)
//! ```
//!
//! The format is specified in the
//! [.knode specification v0](https://github.com/knodemd/format/blob/main/spec/knode-0.md).
//! [`Space`] is the only entry point:
//!
//! ```
//! use knode_format::{NewNode, NodeUpdate, Provenance, Space};
//!
//! # let dir = tempfile::tempdir().unwrap();
//! let mut space = Space::create(dir.path().join("Example.knode"), "Example")?;
//! let me = Provenance::person("Ángel");
//!
//! let roadmap = space.create_node(NewNode::note("Roadmap", "Ship v0.1 this week."), &me)?;
//! let plan = space.create_node(NewNode::note("Plan", "Follow the [[Roadmap]]."), &me)?;
//!
//! // `[[Roadmap]]` became a link, stored by id.
//! assert_eq!(space.backlinks(&roadmap.id)?[0].from, plan.id);
//!
//! // Renaming rewrites the links that point at the node.
//! space.update_node(&roadmap.id, NodeUpdate { title: Some("Roadmap 2026".into()), ..Default::default() }, &me)?;
//! assert_eq!(space.get_node(&plan.id)?.unwrap().body, "Follow the [[Roadmap 2026]].");
//!
//! let hits = space.search("ship", 10)?;
//! assert_eq!(hits[0].node.id, roadmap.id);
//! # Ok::<(), knode_format::Error>(())
//! ```
//!
//! A [`Space`] holds one SQLite connection. It is [`Send`] but not [`Sync`]: share it behind a
//! mutex. At runtime only Knode Service opens a space; tools and tests open it directly.

#![warn(missing_docs)]

mod access;
mod attachments;
mod error;
mod history;
mod links;
mod manifest;
mod schema;
mod space;
mod types;
mod views;

pub use attachments::{Attachment, Blob, LinkTarget, MAX_ATTACHMENT_SIZE, blob_url_hash};
pub use error::{Error, Result};
pub use links::{LinkDirection, LinkKind, LinkKindSpec, NodeLink};
pub use manifest::{FORMAT_VERSION, Manifest};
pub use schema::SCHEMA_VERSION;
pub use space::Space;
pub use types::{
    ActorKind, Agent, AgentKey, Change, ChangeDetail, ChangeId, ChangeOp, Folder, FolderSpec,
    GrantLevel, HistoryEntry, Link, LinkHistoryEntry, NewNode, Node, NodeId, NodeSummary,
    NodeUpdate, Properties, Provenance, SearchHit, Timestamp,
};
pub use views::{
    GraphViewSpec, GroupSpec, Position, PositionSpec, View, ViewDisplay, ViewGroup, Viewport,
};

/// The extension of a space package.
pub const EXTENSION: &str = "knode";
/// The manifest inside a package.
pub const MANIFEST_FILE: &str = "manifest.json";
/// The database inside a package.
pub const DATABASE_FILE: &str = "knode.db";
/// The attachments folder inside a package: one file per blob, named by its hash.
pub const BLOBS_DIR: &str = "blobs";
/// The sync folder inside a package, reserved for now.
pub const SYNC_DIR: &str = "sync";

/// The node kinds the format defines. Any other kind that follows the naming rule
/// (`[a-z][a-z0-9-]*`, at most 32 characters) is kept as it is.
pub mod kinds {
    /// A note written by a person or an agent. The default.
    pub const NOTE: &str = "note";
    /// A conversation with an agent, captured as it happened.
    pub const CONVERSATION: &str = "conversation";
    /// A decision and the reasons for it.
    pub const DECISION: &str = "decision";
    /// An idea, not yet worked out.
    pub const IDEA: &str = "idea";
    /// Something read: a web page, a document, a book.
    pub const SOURCE: &str = "source";
    /// A canvas: its title names it and its body says what it is for (schema version 4). Made
    /// with [`Space::create_canvas`](crate::Space::create_canvas).
    pub const CANVAS: &str = "canvas";
}

/// The property keys the format defines. Any other key that follows the naming rule
/// (`[a-z][a-z0-9_.-]*`, at most 64 characters) is kept as it is.
///
/// Properties hold structured metadata of a node, never written into its Markdown body. See
/// [`Space::set_properties`].
pub mod property_keys {
    /// The agent of a `conversation` node, such as `claude-code` or `codex`.
    pub const CONVERSATION_AGENT: &str = "conversation.agent";
    /// The agent's id for the session of a `conversation` node.
    pub const CONVERSATION_SESSION: &str = "conversation.session";
    /// The folder the conversation was held in.
    pub const CONVERSATION_PROJECT: &str = "conversation.project";
    /// The git branch at the start of the conversation.
    pub const CONVERSATION_BRANCH: &str = "conversation.branch";
    /// The last model that answered.
    pub const CONVERSATION_MODEL: &str = "conversation.model";
    /// When the conversation started, as RFC 3339 in UTC with milliseconds.
    pub const CONVERSATION_STARTED_AT: &str = "conversation.started_at";
    /// When the conversation last had a message, as RFC 3339 in UTC with milliseconds.
    pub const CONVERSATION_ENDED_AT: &str = "conversation.ended_at";
}

/// The link kinds the format defines, built in. From schema version 4 a space defines kinds of
/// its own with [`Space::set_link_kind`](crate::Space::set_link_kind), and a link is made only
/// with a built-in or defined kind. Links of kinds a reader does not know are kept as they are.
pub mod link_kinds {
    /// A `[[link]]` in the body of the source node. Only written by Knode, from the body.
    pub const MENTIONS: &str = "mentions";
    /// Related nodes. The default for links made by hand.
    pub const RELATED: &str = "related";
    /// The source node is a part of the target, such as a note of a project.
    pub const PART_OF: &str = "part-of";
    /// The source node was made from the target, such as a summary of a conversation.
    pub const DERIVED_FROM: &str = "derived-from";
    /// The source node answers a question the target asks (schema version 4).
    pub const ANSWERS: &str = "answers";
    /// The source node says the opposite of the target (schema version 4).
    pub const CONTRADICTS: &str = "contradicts";

    /// Every built-in kind, in the order the specification lists them.
    pub const BUILT_IN: &[&str] = &[
        MENTIONS,
        RELATED,
        PART_OF,
        DERIVED_FROM,
        ANSWERS,
        CONTRADICTS,
    ];
}

/// The view kinds the format defines (schema version 4). Views of other kinds are kept and not
/// shown.
pub mod view_kinds {
    /// The space, or the part of it a filter keeps, as nodes and the links between them.
    pub const GRAPH: &str = "graph";
    /// A board arranged by hand, which is also a node of kind [`canvas`](crate::kinds::CANVAS).
    pub const CANVAS: &str = "canvas";
}

/// The actions of changes the format defines (schema version 3). A reader keeps and shows actions
/// it does not know.
pub mod actions {
    /// A node was created, with its properties, links and tags.
    pub const NODE_CREATE: &str = "node.create";
    /// A node's kind, title, body or tags changed.
    pub const NODE_UPDATE: &str = "node.update";
    /// A node was deleted.
    pub const NODE_DELETE: &str = "node.delete";
    /// A node was put back to an earlier version, or recovered after a delete.
    pub const NODE_RESTORE: &str = "node.restore";
    /// A node and its history were erased.
    pub const NODE_PURGE: &str = "node.purge";
    /// A link was made by hand.
    pub const LINK_CREATE: &str = "link.create";
    /// A link was removed by hand.
    pub const LINK_DELETE: &str = "link.delete";
    /// A change was undone.
    pub const UNDO: &str = "undo";
    /// Many nodes were written at once.
    pub const IMPORT: &str = "import";
    /// A setting of the space changed.
    pub const SETTINGS_SET: &str = "settings.set";
    /// A folder was added or changed.
    pub const FOLDER_SET: &str = "folder.set";
    /// A folder was removed.
    pub const FOLDER_REMOVE: &str = "folder.remove";
    /// An agent was added to the space.
    pub const AGENT_ADD: &str = "agent.add";
    /// A key of an agent was added.
    pub const AGENT_KEY_ADD: &str = "agent.key.add";
    /// A key of an agent was revoked.
    pub const AGENT_KEY_REVOKE: &str = "agent.key.revoke";
    /// The grant of an agent was set.
    pub const GRANT_SET: &str = "grant.set";
    /// A migration rewrote data.
    pub const MIGRATE: &str = "migrate";
    /// A link kind was defined or changed (schema version 4).
    pub const LINK_KIND_SET: &str = "link_kind.set";
    /// A link kind was removed (schema version 4).
    pub const LINK_KIND_REMOVE: &str = "link_kind.remove";
    /// A graph view was created, renamed or filtered (schema version 4).
    pub const VIEW_SET: &str = "view.set";
    /// A graph view was removed (schema version 4).
    pub const VIEW_REMOVE: &str = "view.remove";
    /// A file was attached to a node (schema version 4).
    pub const ATTACHMENT_ADD: &str = "attachment.add";
    /// An attachment was removed from a node; its blob stays (schema version 4).
    pub const ATTACHMENT_REMOVE: &str = "attachment.remove";
    /// Unused blobs were removed, files and rows (schema version 4).
    pub const BLOBS_PURGE: &str = "blobs.purge";
}

/// The folder kinds the format defines (schema version 3). Folders of other kinds are kept and
/// not acted on.
pub mod folder_kinds {
    /// A folder people and agents work in; conversations held in it are captured into the space.
    pub const PROJECT: &str = "project";
    /// A folder of Markdown files imported into the space.
    pub const NOTES: &str = "notes";
    /// A folder where an agent keeps its sessions.
    pub const AGENT: &str = "agent";
}

/// The settings keys the format defines.
pub mod setting_keys {
    /// The id of the space, as in `manifest.json`. Never changes.
    pub const SPACE_ID: &str = "space.id";
    /// The icon of the space: one emoji (schema version 3).
    pub const SPACE_ICON: &str = "space.icon";
    /// `on` or `off`: whether captured conversations are written to the space (schema version 3).
    pub const CAPTURE_ENABLED: &str = "capture.enabled";
    /// The id of the embedding model searches by meaning use, in decimal (schema version 4).
    pub const EMBEDDING_MODEL: &str = "embedding.model";
}
