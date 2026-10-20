//! Reading a session file, all at once or a little at a time while the agent writes it.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::discover::{SessionFile, file_version, read_whole};
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
    /// Records read and applied. A session read whole counts as one record.
    pub records: usize,
    /// Lines (or whole files) that were not valid JSON and were skipped.
    pub skipped: usize,
    /// Messages added to the conversation (the last one may also have grown).
    pub new_messages: usize,
    /// How far reading got: the byte offset of the end of the last complete line, or for a
    /// session read whole, the version read (see [`SessionFile::len`]).
    pub offset: u64,
}

/// Reads a session into a [`Conversation`] and can continue later from where it stopped.
///
/// Sessions that only grow (Claude Code, Codex, Copilot CLI and Gemini CLI `.jsonl` files, Aider
/// history files) are read a line at a time; a last line still being written is left for the
/// next poll. Sessions that are rewritten (Gemini CLI `.json` files, Amp threads, OpenCode
/// sessions, Cursor and Windsurf conversations, Cline and Roo Code tasks) are read whole again
/// whenever they change.
///
/// An Aider history file holds one session per run of Aider: the reader follows the newest, and
/// hands over the ones that ended with [`SessionReader::take_finished`].
#[derive(Debug, Clone)]
pub struct SessionReader {
    path: PathBuf,
    offset: u64,
    conversation: Conversation,
    finished: Vec<Conversation>,
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
            finished: Vec::new(),
        }
    }

    /// The file being read.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How far reading got: the end of the last complete line read, or the version of a
    /// session read whole.
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

    /// The conversations that ended since the last call, oldest first: the earlier sessions of
    /// an Aider history file. Always empty for other agents.
    pub fn take_finished(&mut self) -> Vec<Conversation> {
        std::mem::take(&mut self.finished)
    }

    /// Reads what was written since the last poll. Lines that are not valid JSON are skipped,
    /// except an unfinished last line, which is read again next time.
    pub fn poll(&mut self) -> Result<Progress, Error> {
        let agent = self.conversation.agent;
        if matches!(agent, Agent::OpenCode | Agent::Cursor | Agent::Windsurf) {
            return self.poll_database();
        }
        if read_whole(agent, &self.path) {
            return self.poll_whole();
        }
        let progress = self.poll_lines()?;
        if agent == Agent::GeminiCli && self.conversation.project.is_none() {
            self.conversation.project = crate::gemini::project_root(&self.path);
        }
        Ok(progress)
    }

    fn unchanged(&self) -> Progress {
        Progress {
            records: 0,
            skipped: 0,
            new_messages: 0,
            offset: self.offset,
        }
    }

    /// Reads a session file that is rewritten as it grows, whole, if it changed.
    fn poll_whole(&mut self) -> Result<Progress, Error> {
        let version = file_version(&fs::metadata(&self.path)?);
        if version == self.offset && self.offset != 0 {
            return Ok(self.unchanged());
        }
        let bytes = fs::read(&self.path)?;
        let Ok(document) = serde_json::from_slice::<Value>(&bytes) else {
            // Probably caught halfway through a write: try again next time.
            let mut progress = self.unchanged();
            progress.skipped = 1;
            return Ok(progress);
        };
        let agent = self.conversation.agent;
        let mut conversation = Conversation::new(agent, self.conversation.session_id.clone());
        match agent {
            Agent::GeminiCli => {
                crate::gemini::apply_document(&mut conversation, &document);
                conversation.project = crate::gemini::project_root(&self.path);
            }
            Agent::Amp => crate::amp::apply_document(&mut conversation, &document),
            Agent::Cline | Agent::RooCode => crate::cline::apply_document(
                &mut conversation,
                &document,
                &self.path,
                self.conversation.project.clone(),
            ),
            _ => {}
        }
        conversation.forget_records();
        Ok(self.replace(conversation, version))
    }

    /// Reads a session kept in a database (OpenCode, Cursor, Windsurf), whole, if it changed.
    fn poll_database(&mut self) -> Result<Progress, Error> {
        let agent = self.conversation.agent;
        let version = match agent {
            Agent::Cursor => crate::cursor::version(&self.path)?,
            Agent::Windsurf => crate::windsurf::version(&self.path)?,
            _ => crate::opencode::version(&self.path)?,
        };
        let Some(version) = version else {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!("the {agent} session was deleted"),
            )));
        };
        if version == self.offset && self.offset != 0 {
            return Ok(self.unchanged());
        }
        let mut conversation = Conversation::new(agent, self.conversation.session_id.clone());
        // What is read is the version read, even if the database moved on in between.
        let version = match agent {
            Agent::Cursor => crate::cursor::read(&self.path, &mut conversation)?,
            Agent::Windsurf => crate::windsurf::read(&self.path, &mut conversation)?,
            _ => {
                crate::opencode::read(&self.path, &mut conversation)?;
                version
            }
        };
        Ok(self.replace(conversation, version))
    }

    fn replace(&mut self, conversation: Conversation, version: u64) -> Progress {
        let before = self.conversation.messages.len();
        let after = conversation.messages.len();
        self.conversation = conversation;
        self.offset = version;
        Progress {
            records: 1,
            skipped: 0,
            new_messages: after.saturating_sub(before),
            offset: version,
        }
    }

    /// Reads the lines written since the last poll.
    fn poll_lines(&mut self) -> Result<Progress, Error> {
        let agent = self.conversation.agent;
        if agent == Agent::Aider && self.conversation.project.is_none() {
            self.conversation.project = Some(crate::aider::project_of(&self.path).to_path_buf());
        }
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
            if agent == Agent::Aider {
                if !complete {
                    break;
                }
                let text = String::from_utf8_lossy(&line);
                crate::aider::apply_line(
                    &mut self.conversation,
                    text.trim_end_matches(['\n', '\r']),
                    &self.path,
                    &mut self.finished,
                );
                progress.records += 1;
            } else if trimmed.is_empty() {
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
        progress.new_messages = self
            .conversation
            .messages
            .len()
            .saturating_sub(messages_before);
        Ok(progress)
    }
}

/// Reads a whole session file into a conversation. For an Aider history file, which holds a
/// session per run, this is the newest; [`read_sessions`] gives them all.
pub fn read_session(file: &SessionFile) -> Result<Conversation, Error> {
    let mut reader = SessionReader::new(file);
    reader.poll()?;
    Ok(reader.into_conversation())
}

/// Reads every conversation in a session file, oldest first: one, except for an Aider history
/// file, which holds one per run of Aider. Conversations with no messages are left out.
pub fn read_sessions(file: &SessionFile) -> Result<Vec<Conversation>, Error> {
    let mut reader = SessionReader::new(file);
    reader.poll()?;
    let mut conversations = reader.take_finished();
    conversations.push(reader.into_conversation());
    conversations.retain(|conversation| !conversation.is_empty());
    Ok(conversations)
}
