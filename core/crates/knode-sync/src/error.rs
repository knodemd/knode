//! Errors of co-editing.

use knode_format::NodeId;

/// An error while editing a node's body through its Loro document.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Reading or writing the space failed.
    #[error(transparent)]
    Format(#[from] knode_format::Error),
    /// Reading or writing a file in `sync/` failed.
    #[error("sync folder: {0}")]
    Io(#[from] std::io::Error),
    /// Loro refused an operation.
    #[error("Loro: {0}")]
    Loro(String),
    /// No node has this id.
    #[error("no node with id {0}")]
    NotFound(NodeId),
    /// An edit or a version is not valid, such as a position past the end of the text.
    #[error("{0}")]
    Invalid(String),
    /// Updates were made as a Loro peer that the space does not know. Every peer is recorded in
    /// `loro_peers` before edits are made as it, so authorship is kept.
    #[error("updates made as an unknown Loro peer {0}")]
    UnknownPeer(u64),
    /// Updates depend on edits the document does not have yet; send them from an earlier
    /// version.
    #[error("the updates depend on edits the document does not have")]
    MissingHistory,
    /// An edit was based on a version the document does not have.
    #[error("the document has no version {0}")]
    UnknownVersion(String),
}

impl From<loro::LoroError> for Error {
    fn from(e: loro::LoroError) -> Self {
        Error::Loro(e.to_string())
    }
}

impl From<loro::LoroEncodeError> for Error {
    fn from(e: loro::LoroEncodeError) -> Self {
        Error::Loro(e.to_string())
    }
}

/// A result of co-editing.
pub type Result<T, E = Error> = std::result::Result<T, E>;
