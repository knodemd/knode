use std::io;
use std::path::PathBuf;

use crate::protocol::RpcError;

/// Everything that can go wrong running Knode Service or talking to it.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A file or the socket could not be read or written.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// Another Knode Service already holds the lock in this Knode home.
    #[error("Knode Service is already running{}", match .pid { Some(pid) => format!(" (pid {pid})"), None => String::new() })]
    AlreadyRunning {
        /// The process id written in the lock file, when it could be read.
        pid: Option<u32>,
    },
    /// The Knode home could not be found: neither `KNODE_HOME` nor a home folder is set.
    #[error("cannot find the Knode home: set KNODE_HOME")]
    NoHome,
    /// The service answered with an error.
    #[error("{0}")]
    Rpc(#[from] RpcError),
    /// The service sent something that is not valid for the protocol.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The connection to the service closed before the answer came.
    #[error("the connection to Knode Service closed")]
    Disconnected,
    /// The service that was started exited without listening.
    #[error("Knode Service could not start ({status})")]
    StartFailed {
        /// How it exited.
        status: String,
    },
    /// The service did not start in time.
    #[error("Knode Service did not start within {seconds} seconds at {socket}")]
    StartTimeout {
        /// How long the client waited.
        seconds: u64,
        /// The socket the client waited for.
        socket: PathBuf,
    },
}

/// A `Result` whose error is [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Protocol(e.to_string())
    }
}
