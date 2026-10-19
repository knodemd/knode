//! Capture conversations from AI agents.
//!
//! Every Claude Code and Codex conversation becomes a [`Conversation`]: its agent, project, dates,
//! title and turns, with tool calls condensed to one line each and tool output left out. A
//! conversation renders to Markdown with [`to_markdown`]; its metadata stays structured.
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

mod capture;
pub mod claude_code;
pub mod codex;
mod discover;
mod filter;
mod model;
mod reader;
mod render;
mod text;
mod watch;

pub use capture::{Capture, CaptureError, CaptureReport, Captured};
pub use discover::{SessionFile, default_root, discover, discover_all};
pub use filter::ProjectFilter;
pub use model::{Agent, Conversation, Message, Part, Role, Title, TitleSource, ToolCall};
pub use reader::{Error, Progress, SessionReader, read_session};
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
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn has_a_version() {
        assert!(!super::version().is_empty());
    }
}
