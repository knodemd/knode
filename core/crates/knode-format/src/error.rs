use std::path::PathBuf;

use crate::NodeId;

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
}

/// A `Result` whose error is [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;
