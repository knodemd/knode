use std::path::PathBuf;

/// Everything that can go wrong while getting a model, loading it or embedding text.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Reading or writing a file failed.
    #[error("{path}: {source}")]
    Io {
        /// The file or folder involved.
        path: PathBuf,
        /// What the system reported.
        source: std::io::Error,
    },
    /// Knode does not know where its home folder is.
    #[error(transparent)]
    Config(#[from] knode_config::Error),
    /// A model file could not be downloaded.
    #[error("could not download {file}: {message}")]
    Download {
        /// The file being downloaded.
        file: String,
        /// Why it failed.
        message: String,
    },
    /// A downloaded file does not match its expected SHA-256, so it was thrown away.
    #[error("{file} is not the expected file: SHA-256 {actual}, expected {expected}")]
    Checksum {
        /// The file that failed the check.
        file: String,
        /// The SHA-256 it should have.
        expected: String,
        /// The SHA-256 it has.
        actual: String,
    },
    /// The model folder is incomplete or its files are not valid.
    #[error("invalid model in {path}: {message}")]
    InvalidModel {
        /// The model folder.
        path: PathBuf,
        /// What is wrong with it.
        message: String,
    },
    /// The tokenizer failed on some text.
    #[error("tokenizer: {0}")]
    Tokenizer(String),
    /// Running the model failed.
    #[error("inference: {0}")]
    Inference(#[from] candle_core::Error),
}

/// A `Result` whose error is [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn invalid(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Error::InvalidModel {
            path: path.into(),
            message: message.into(),
        }
    }
}
