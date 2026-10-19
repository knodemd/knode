//! Reading a session file, all at once or a little at a time while the agent writes it.

use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::discover::SessionFile;
use crate::model::{Agent, Conversation};

/// Why a session could not be read.
#[derive(Debug)]
pub enum Error {
    /// The file could not be opened or read.
    Io(io::Error),
    /// The file is now shorter than the offset reading had reached, so it was replaced or cut.
    /// Start again from offset zero with a new conversation.
    Truncated {
        /// The offset reading had reached.
        offset: u64,
        /// The file's length now.
        len: u64,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(error) => write!(f, "could not read the session: {error}"),
            Error::Truncated { offset, len } => write!(
                f,
                "the session file is {len} bytes, shorter than the {offset} already read"
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(error) => Some(error),
            Error::Truncated { .. } => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error::Io(error)
    }
}

/// What one [`SessionReader::poll`] read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Records read and applied.
    pub records: usize,
    /// Lines that were not valid JSON and were skipped.
    pub skipped: usize,
    /// Messages added to the conversation (the last one may also have grown).
    pub new_messages: usize,
    /// The byte offset reading reached: the end of the last complete line.
    pub offset: u64,
}

/// Reads a session file into a [`Conversation`], line by line, and can continue later from where
/// it stopped. A last line still being written is left for the next poll.
#[derive(Debug, Clone)]
pub struct SessionReader {
    path: PathBuf,
    offset: u64,
    conversation: Conversation,
}

impl SessionReader {
    /// A reader for a discovered session file, starting at its beginning.
    pub fn new(file: &SessionFile) -> Self {
        Self::open(file.agent, &file.path, file.session_id.clone())
    }

    /// A reader for any session file of `agent`, starting at its beginning.
    pub fn open(agent: Agent, path: impl AsRef<Path>, session_id: impl Into<String>) -> Self {
        Self::resume(path, Conversation::new(agent, session_id), 0)
    }

    /// A reader that continues a previous read: `conversation` and `offset` are what an earlier
    /// reader had reached (see [`SessionReader::conversation`] and [`SessionReader::offset`]).
    pub fn resume(path: impl AsRef<Path>, conversation: Conversation, offset: u64) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            offset,
            conversation,
        }
    }

    /// The file being read.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The byte offset reached: the end of the last complete line read.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// The conversation read so far.
    pub fn conversation(&self) -> &Conversation {
        &self.conversation
    }

    /// The conversation read so far, consuming the reader.
    pub fn into_conversation(self) -> Conversation {
        self.conversation
    }

    /// Reads the lines written since the last poll. Lines that are not valid JSON are skipped,
    /// except an unfinished last line, which is read again next time.
    pub fn poll(&mut self) -> Result<Progress, Error> {
        let mut file = File::open(&self.path)?;
        let len = file.metadata()?.len();
        if len < self.offset {
            return Err(Error::Truncated {
                offset: self.offset,
                len,
            });
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut reader = BufReader::new(file);
        let messages_before = self.conversation.messages.len();
        let mut progress = Progress {
            records: 0,
            skipped: 0,
            new_messages: 0,
            offset: self.offset,
        };
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = reader.read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }
            let complete = line.last() == Some(&b'\n');
            let trimmed = line.trim_ascii();
            if trimmed.is_empty() {
                if !complete {
                    break;
                }
            } else {
                match serde_json::from_slice::<Value>(trimmed) {
                    Ok(record) => {
                        crate::apply(&mut self.conversation, &record);
                        progress.records += 1;
                    }
                    Err(_) if !complete => break,
                    Err(_) => progress.skipped += 1,
                }
            }
            self.offset += read as u64;
        }
        progress.offset = self.offset;
        progress.new_messages = self.conversation.messages.len() - messages_before;
        Ok(progress)
    }
}

/// Reads a whole session file into a conversation.
pub fn read_session(file: &SessionFile) -> Result<Conversation, Error> {
    let mut reader = SessionReader::new(file);
    reader.poll()?;
    Ok(reader.into_conversation())
}
