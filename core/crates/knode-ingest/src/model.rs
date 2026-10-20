//! The neutral conversation model every capture source produces.

use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The agent a conversation was held with.
///
/// More agents may be added, so matches on it need a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Agent {
    /// Claude Code, which keeps sessions in `~/.claude/projects`.
    ClaudeCode,
    /// Codex, which keeps sessions in `~/.codex/sessions`.
    Codex,
    /// Gemini CLI, which keeps sessions in `~/.gemini/tmp/<project>/chats`.
    #[serde(rename = "gemini-cli")]
    GeminiCli,
    /// GitHub Copilot CLI, which keeps sessions in `~/.copilot/session-state`.
    #[serde(rename = "copilot-cli")]
    CopilotCli,
    /// OpenCode, which keeps sessions in a SQLite database in `~/.local/share/opencode`.
    #[serde(rename = "opencode")]
    OpenCode,
    /// Amp, which keeps threads in `~/.local/share/amp/threads`.
    Amp,
    /// Aider, which keeps a `.aider.chat.history.md` file in each project.
    Aider,
    /// Cursor, which keeps chats and agent sessions in the SQLite database `state.vscdb` in
    /// its `User/globalStorage` folder.
    Cursor,
    /// Windsurf, which keeps workspace chats in the SQLite database `state.vscdb` of each
    /// folder in its `User/workspaceStorage` folder.
    Windsurf,
    /// Cline, the VS Code extension, which keeps one folder per task in its `globalStorage`
    /// folder (`saoudrizwan.claude-dev/tasks`).
    Cline,
    /// Roo Code, the VS Code extension, which keeps tasks the way Cline does
    /// (`rooveterinaryinc.roo-cline/tasks`).
    #[serde(rename = "roo-code")]
    RooCode,
    /// Claude on the web and in the Claude apps, imported from a claude.ai data export.
    ClaudeAi,
    /// ChatGPT, imported from a ChatGPT data export.
    #[serde(rename = "chatgpt")]
    ChatGpt,
    /// Gemini on the web and in the Gemini apps, imported from Google Takeout.
    GeminiWeb,
}

impl Agent {
    /// Every agent capture reads sessions from, in a stable order. Agents whose conversations
    /// only come from an export are in [`Agent::EXPORTED`].
    pub const ALL: [Agent; 11] = [
        Agent::ClaudeCode,
        Agent::Codex,
        Agent::GeminiCli,
        Agent::CopilotCli,
        Agent::OpenCode,
        Agent::Amp,
        Agent::Aider,
        Agent::Cursor,
        Agent::Windsurf,
        Agent::Cline,
        Agent::RooCode,
    ];

    /// The agents whose conversations are imported from a data export (see
    /// [`import_export`](crate::import_export)), in a stable order.
    pub const EXPORTED: [Agent; 3] = [Agent::ClaudeAi, Agent::ChatGpt, Agent::GeminiWeb];

    /// A stable identifier, suitable for storage.
    pub fn id(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude-code",
            Agent::Codex => "codex",
            Agent::GeminiCli => "gemini-cli",
            Agent::CopilotCli => "copilot-cli",
            Agent::OpenCode => "opencode",
            Agent::Amp => "amp",
            Agent::Aider => "aider",
            Agent::Cursor => "cursor",
            Agent::Windsurf => "windsurf",
            Agent::Cline => "cline",
            Agent::RooCode => "roo-code",
            Agent::ClaudeAi => "claude-ai",
            Agent::ChatGpt => "chatgpt",
            Agent::GeminiWeb => "gemini-web",
        }
    }

    /// The agent with this [`Agent::id`].
    pub fn from_id(id: &str) -> Option<Agent> {
        Agent::ALL
            .into_iter()
            .chain(Agent::EXPORTED)
            .find(|agent| agent.id() == id)
    }

    /// The name people know the agent by.
    pub fn name(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Claude Code",
            Agent::Codex => "Codex",
            Agent::GeminiCli => "Gemini CLI",
            Agent::CopilotCli => "Copilot CLI",
            Agent::OpenCode => "OpenCode",
            Agent::Amp => "Amp",
            Agent::Aider => "Aider",
            Agent::Cursor => "Cursor",
            Agent::Windsurf => "Windsurf",
            Agent::Cline => "Cline",
            Agent::RooCode => "Roo Code",
            Agent::ClaudeAi => "Claude",
            Agent::ChatGpt => "ChatGPT",
            Agent::GeminiWeb => "Gemini",
        }
    }
}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Who wrote a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// The person using the agent.
    User,
    /// The agent.
    Assistant,
}

/// One tool call, condensed to a single line. Tool output is never kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The id the agent gave the call, used to match its result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The tool's name, for example `Bash` or `exec_command`.
    pub name: String,
    /// A one-line summary of the input, for example the command that ran.
    pub summary: String,
    /// Whether the tool reported an error.
    #[serde(default)]
    pub failed: bool,
}

/// One piece of a message, in the order it was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Part {
    /// Markdown text.
    Text {
        /// The text, as the author wrote it.
        text: String,
    },
    /// A tool call.
    Tool(ToolCall),
    /// An image that was attached; its data is not kept.
    Image,
}

/// One turn of the conversation. Consecutive records from the same role are merged, so an
/// assistant turn holds all its text and tool calls up to the next user message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Who wrote it.
    pub role: Role,
    /// When the turn started, if the source recorded it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
    /// The text, tool calls and images of the turn, in order.
    pub parts: Vec<Part>,
}

impl Message {
    /// A message with no parts yet.
    pub fn new(role: Role, timestamp: Option<DateTime<Utc>>) -> Self {
        Self {
            role,
            timestamp,
            parts: Vec::new(),
        }
    }

    /// All the text of the message, paragraphs separated by a blank line.
    pub fn text(&self) -> String {
        let texts: Vec<&str> = self
            .parts
            .iter()
            .filter_map(|part| match part {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        texts.join("\n\n")
    }

    /// The tool calls of the message, in order.
    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.parts.iter().filter_map(|part| match part {
            Part::Tool(call) => Some(call),
            _ => None,
        })
    }

    /// Whether the message has nothing to show.
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }
}

/// Where a title came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TitleSource {
    /// The person named the conversation.
    User,
    /// The agent generated a title.
    Agent,
}

/// A title recorded by the agent or the person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Title {
    /// The title.
    pub text: String,
    /// Who gave it.
    pub source: TitleSource,
}

/// One conversation with an agent, in a form that does not depend on the agent.
///
/// It is also the state of an incremental read: keep it with the byte offset, and reading can
/// continue later from where it stopped (see [`crate::SessionReader::resume`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    /// The agent.
    pub agent: Agent,
    /// The agent's id for the session.
    pub session_id: String,
    /// The working directory the conversation was held in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<PathBuf>,
    /// The git branch at the start, if recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    /// The last model that answered, if recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// When the conversation started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    /// The time of the last record read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    /// The title recorded in the session, if any. See [`Conversation::title`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_title: Option<Title>,
    /// The turns, in order.
    pub messages: Vec<Message>,
    /// Where each record of a source that rewrites or rewinds records began, so it can be
    /// replaced (see [`Conversation::begin_record`]).
    #[serde(
        default,
        rename = "record_marks",
        skip_serializing_if = "Vec::is_empty"
    )]
    marks: Vec<RecordMark>,
}

/// Where one record began in a conversation: how many messages there were, and how many parts
/// the last of them had.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RecordMark {
    id: String,
    messages: usize,
    parts: usize,
}

impl Conversation {
    /// An empty conversation.
    pub fn new(agent: Agent, session_id: impl Into<String>) -> Self {
        Self {
            agent,
            session_id: session_id.into(),
            project: None,
            git_branch: None,
            model: None,
            started_at: None,
            ended_at: None,
            recorded_title: None,
            messages: Vec::new(),
            marks: Vec::new(),
        }
    }

    /// The title: the one recorded in the session, or one derived from the first user message.
    pub fn title(&self) -> String {
        match &self.recorded_title {
            Some(title) if !title.text.trim().is_empty() => title.text.trim().to_string(),
            _ => crate::render::derive_title(self),
        }
    }

    /// Whether the conversation has no messages, for example a session that was opened and
    /// closed without a prompt. Such conversations are usually not worth keeping.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Notes the time of a record, widening `started_at` and `ended_at`.
    pub(crate) fn saw_time(&mut self, at: DateTime<Utc>) {
        if self.started_at.is_none_or(|start| at < start) {
            self.started_at = Some(at);
        }
        if self.ended_at.is_none_or(|end| at > end) {
            self.ended_at = Some(at);
        }
    }

    /// Adds a part to the last message if it has the same role, or starts a new message.
    pub(crate) fn push_part(&mut self, role: Role, at: Option<DateTime<Utc>>, part: Part) {
        match self.messages.last_mut() {
            Some(last) if last.role == role => {
                if last.timestamp.is_none() {
                    last.timestamp = at;
                }
                last.parts.push(part);
            }
            _ => {
                let mut message = Message::new(role, at);
                message.parts.push(part);
                self.messages.push(message);
            }
        }
    }

    /// Starts the record `id` of a source that writes a record again when it changes (Gemini
    /// CLI). A record already seen last is taken back first, so applying it again replaces it.
    /// Returns `false` for a record seen earlier than the last one, which is left as it was.
    pub(crate) fn begin_record(&mut self, id: &str) -> bool {
        if let Some(index) = self.marks.iter().position(|mark| mark.id == id) {
            if index + 1 != self.marks.len() {
                return false;
            }
            self.rewind_to(index);
        }
        self.marks.push(RecordMark {
            id: id.to_string(),
            messages: self.messages.len(),
            parts: self.messages.last().map_or(0, |m| m.parts.len()),
        });
        true
    }

    /// Takes back the record `id` and every record after it, as when the person rewinds the
    /// conversation. An unknown id changes nothing.
    pub(crate) fn rewind(&mut self, id: &str) {
        if let Some(index) = self.marks.iter().position(|mark| mark.id == id) {
            self.rewind_to(index);
        }
    }

    fn rewind_to(&mut self, index: usize) {
        let mark = &self.marks[index];
        self.messages.truncate(mark.messages);
        if let Some(last) = self.messages.last_mut() {
            last.parts.truncate(mark.parts);
        }
        self.marks.truncate(index);
    }

    /// Forgets the record marks, for a source read whole.
    pub(crate) fn forget_records(&mut self) {
        self.marks.clear();
    }

    /// Adds a line of text to the last text part if it has the same role, or starts one.
    pub(crate) fn push_line(&mut self, role: Role, at: Option<DateTime<Utc>>, line: &str) {
        if let Some(last) = self.messages.last_mut() {
            if last.role == role {
                if let Some(Part::Text { text }) = last.parts.last_mut() {
                    text.push('\n');
                    text.push_str(line);
                    return;
                }
            }
        }
        self.push_part(
            role,
            at,
            Part::Text {
                text: line.to_string(),
            },
        );
    }

    /// Marks the tool call with this id as failed, searching from the end.
    pub(crate) fn mark_failed(&mut self, id: &str) {
        for message in self.messages.iter_mut().rev() {
            for part in message.parts.iter_mut().rev() {
                if let Part::Tool(call) = part {
                    if call.id.as_deref() == Some(id) {
                        call.failed = true;
                        return;
                    }
                }
            }
        }
    }
}
