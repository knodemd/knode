//! Capture conversations from AI agents.
//!
//! Every conversation with Claude Code, Codex, Gemini CLI, Copilot CLI, OpenCode, Amp, Aider,
//! Cursor, Windsurf, Cline or Roo Code becomes a [`Conversation`]: its agent, project, dates, title and turns, with tool calls
//! condensed to one line each and tool output left out. A conversation renders to Markdown with
//! [`to_markdown`]; its metadata stays structured. [`default_root`] says where each agent keeps
//! its sessions.
//!
//! ```no_run
//! use knode_ingest::{ProjectFilter, discover_all, read_session, to_markdown};
//!
//! let filter = ProjectFilter::excluding(["/Users/example/private"]);
//! for file in discover_all()? {
//!     if !filter.may_allow_file(&file) {
//!         continue;
//!     }
//!     let conversation = read_session(&file)?;
//!     if conversation.is_empty() || !filter.allows(&conversation) {
//!         continue;
//!     }
//!     println!("{}", to_markdown(&conversation));
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! To follow a file while the agent writes it, keep a [`SessionReader`] and call
//! [`SessionReader::poll`] when the file changes, or store its conversation and offset and
//! continue later with [`SessionReader::resume`].
//!
//! To keep a space current, [`Capture`] writes one `conversation` node per session and updates
//! it as the session grows, saving what it has read in a state file; [`Watcher`] follows the
//! session folders and hands [`Capture`] the files that changed.

mod aider;
mod amp;
mod capture;
pub mod claude_code;
mod cline;
pub mod codex;
mod copilot;
mod cursor;
mod discover;
mod filter;
mod gemini;
mod model;
mod opencode;
mod reader;
mod render;
mod text;
mod vscdb;
mod watch;
mod windsurf;

pub use aider::HISTORY_FILE as AIDER_HISTORY_FILE;
pub use capture::{Capture, CaptureError, CaptureReport, Captured};
pub use discover::{
    AIDER_ROOT_VARIABLE, CLINE_ROOT_VARIABLE, CURSOR_ROOT_VARIABLE, ROO_CODE_ROOT_VARIABLE,
    SessionFile, WINDSURF_ROOT_VARIABLE, default_root, default_roots, discover, discover_all,
};
pub use filter::ProjectFilter;
pub use model::{Agent, Conversation, Message, Part, Role, Title, TitleSource, ToolCall};
pub use reader::{Error, Progress, SessionReader, read_session, read_sessions};
pub use render::{derive_title, to_markdown};
pub use watch::{Changes, WatchOptions, Watcher};

/// The version of this crate.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Applies one parsed record to a conversation, with the parser of its agent.
pub(crate) fn apply(conversation: &mut Conversation, record: &serde_json::Value) {
    match conversation.agent {
        Agent::ClaudeCode => claude_code::apply(conversation, record),
        Agent::Codex => codex::apply(conversation, record),
        Agent::GeminiCli => gemini::apply(conversation, record),
        Agent::CopilotCli => copilot::apply(conversation, record),
        // Read whole or as text, never a JSON record at a time.
        Agent::OpenCode
        | Agent::Amp
        | Agent::Aider
        | Agent::Cursor
        | Agent::Windsurf
        | Agent::Cline
        | Agent::RooCode => {}
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn has_a_version() {
        assert!(!super::version().is_empty());
    }
}
