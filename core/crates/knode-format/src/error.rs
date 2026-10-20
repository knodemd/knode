use std::path::PathBuf;

use crate::{ChangeId, NodeId};

/// Everything that can go wrong reading or writing a space.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A file in the package could not be read or written.
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
    /// `knode.db` returned an error.
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// `manifest.json` is missing a field or is not valid JSON.
    #[error("invalid manifest.json: {0}")]
    Manifest(String),
    /// A space cannot be created where one already exists.
    #[error("{0} already exists")]
    AlreadyExists(PathBuf),
    /// The path is not a `.knode` package.
    #[error("{0} is not a .knode package")]
    NotASpace(PathBuf),
    /// The space uses a format version this library does not read.
    #[error("format version {found} is not supported (this library reads version {supported})")]
    UnsupportedFormat {
        /// The `format` in `manifest.json`.
        found: u64,
        /// The format version this library reads and writes.
        supported: u64,
    },
    /// `knode.db` was written by a newer version of Knode.
    #[error("knode.db has schema version {found}, newer than {supported}; update Knode")]
    NewerSchema {
        /// The `user_version` of the database.
        found: u32,
        /// The newest schema version this library knows.
        supported: u32,
    },
    /// `knode.db` belongs to a different space than `manifest.json`.
    #[error("knode.db belongs to space {found}, but manifest.json names {expected}")]
    MismatchedSpace {
        /// The id in `manifest.json`.
        expected: String,
        /// The id stored in `knode.db`.
        found: String,
    },
    /// No node has this id.
    #[error("node {0} not found")]
    NotFound(NodeId),
    /// An argument broke a rule of the format, such as an empty title or a bad kind.
    #[error("{0}")]
    Invalid(String),
    /// No change has this id.
    #[error("change {0} not found")]
    ChangeNotFound(ChangeId),
    /// The change cannot be undone: its action cannot be undone, or the versions it needs were
    /// purged.
    #[error("change {change} cannot be undone: {reason}")]
    CannotUndo {
        /// The change.
        change: ChangeId,
        /// Why not.
        reason: String,
    },
    /// The change was already undone, and that undo has not been undone.
    #[error("change {change} was already undone by change {undone_by}")]
    AlreadyUndone {
        /// The change.
        change: ChangeId,
        /// The undo.
        undone_by: ChangeId,
    },
    /// Later changes touched what the change touched; undoing it would overwrite them.
    #[error(
        "undoing change {change} would overwrite later changes {}",
        later.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
    )]
    UndoConflict {
        /// The change.
        change: ChangeId,
        /// The later changes, oldest first.
        later: Vec<ChangeId>,
    },
    /// A link kind cannot be removed, or change direction, while links have it.
    #[error("link kind {kind} is used by {links} links; remove them first")]
    LinkKindInUse {
        /// The kind.
        kind: String,
        /// How many links have it.
        links: u64,
    },
    /// Search by meaning cannot run here: sqlite-vec is not loaded, or the space has no
    /// embedding model in use (schema version 4, spec section 19).
    #[error("search by meaning is not available: {0}")]
    VectorSearchUnavailable(String),
    /// Only a person may do this, never an agent.
    #[error("only a person can {0}")]
    PersonOnly(String),
}

/// A `Result` whose error is [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;
