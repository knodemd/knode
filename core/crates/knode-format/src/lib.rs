//! Read and write `.knode` spaces: nodes, links, provenance, history and search.
//!
//! A space is a `.knode` package, a folder the Finder shows as one file:
//!
//! ```text
//! Example.knode/
//!   manifest.json   format version, id and name
//!   knode.db        SQLite in WAL mode: nodes, properties, links, provenance, history, search
//!   blobs/          attachments by hash (reserved)
//!   sync/           Loro updates (reserved)
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

mod error;
mod manifest;
mod schema;
mod space;
mod types;

pub use error::{Error, Result};
pub use manifest::{FORMAT_VERSION, Manifest};
pub use schema::SCHEMA_VERSION;
pub use space::Space;
pub use types::{
    ActorKind, ChangeOp, HistoryEntry, Link, NewNode, Node, NodeId, NodeSummary, NodeUpdate,
    Properties, Provenance, SearchHit, Timestamp,
};

/// The extension of a space package.
pub const EXTENSION: &str = "knode";
/// The manifest inside a package.
pub const MANIFEST_FILE: &str = "manifest.json";
/// The database inside a package.
pub const DATABASE_FILE: &str = "knode.db";
/// The attachments folder inside a package, reserved for now.
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

/// The link kinds the format defines. Any other kind that follows the naming rule
/// (`[a-z][a-z0-9-]*`, at most 32 characters) is kept as it is.
pub mod link_kinds {
    /// A `[[link]]` in the body of the source node. Only written by Knode, from the body.
    pub const MENTIONS: &str = "mentions";
    /// Related nodes. The default for links made by hand.
    pub const RELATED: &str = "related";
    /// The source node is a part of the target, such as a note of a project.
    pub const PART_OF: &str = "part-of";
    /// The source node was made from the target, such as a summary of a conversation.
    pub const DERIVED_FROM: &str = "derived-from";
}
