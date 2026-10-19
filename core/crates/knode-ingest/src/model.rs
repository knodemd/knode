//! The neutral conversation model every capture source produces.

use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The agent a conversation was held with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    /// Claude Code, which keeps sessions in `~/.claude/projects`.
    ClaudeCode,
    /// Codex, which keeps sessions in `~/.codex/sessions`.
    Codex,
}

impl Agent {
    /// A stable identifier, suitable for storage.
    pub fn id(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude-code",
            Agent::Codex => "codex",
        }
    }

    /// The name people know the agent by.
    pub fn name(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Claude Code",
            Agent::Codex => "Codex",
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
