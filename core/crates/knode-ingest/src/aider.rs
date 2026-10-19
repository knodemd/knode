//! Aider chat history: `.aider.chat.history.md` in each project folder.
//!
//! Unverified format: no Aider history file was available to check this reader against, so it
//! follows the format Aider documents and writes in its source, read tolerantly. The file is
//! Markdown that only grows, and holds every session run in the project:
//!
//! - `# aider chat started at YYYY-MM-DD HH:MM:SS` (local time) starts a session;
//! - lines starting with `#### ` are what the person typed, one line each;
//! - lines starting with `>` are Aider's own output (edits applied, commits, questions, token
//!   counts). Applied edits, commits and commands run become tool calls; the rest is left out;
//! - everything else is the model's reply.
//!
//! Each session becomes its own conversation, with an id made from the project folder and the
//! start time. Lines carry no times, so a conversation's start is its session's start.

use std::path::Path;

use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};

use crate::model::{Agent, Conversation, Part, Role, ToolCall};
use crate::text::{SUMMARY_MAX, one_line};

/// The name of the history file Aider writes in a project.
pub const HISTORY_FILE: &str = ".aider.chat.history.md";

const SESSION_HEADER: &str = "# aider chat started at ";

/// The session id of a history file before any session header is read.
pub(crate) fn file_session_id(path: &Path) -> String {
    format!("aider-{:016x}", fnv1a(&project_of(path).to_string_lossy()))
}

/// The project folder of a history file: the folder it is in.
pub(crate) fn project_of(path: &Path) -> &Path {
    path.parent().unwrap_or(path)
}

/// Applies one line of the history file. A session header moves the conversation read so far
/// to `finished`, if it has messages, and starts the next one.
pub(crate) fn apply_line(
    conversation: &mut Conversation,
    line: &str,
    path: &Path,
    finished: &mut Vec<Conversation>,
) {
    let line = line.trim_end();
    if let Some(started) = line.strip_prefix(SESSION_HEADER) {
        let started = started.trim();
        let project = project_of(path);
        let key = fnv1a(&project.to_string_lossy());
        let id = match NaiveDateTime::parse_from_str(started, "%Y-%m-%d %H:%M:%S") {
            Ok(naive) => format!("aider-{key:016x}-{}", naive.format("%Y%m%dT%H%M%S")),
            Err(_) => format!("aider-{key:016x}-{}", fnv1a(started)),
        };
        let mut next = Conversation::new(Agent::Aider, id);
        next.project = Some(project.to_path_buf());
        if let Some(at) = local_time(started) {
            next.saw_time(at);
        }
        let previous = std::mem::replace(conversation, next);
        if !previous.is_empty() {
            finished.push(previous);
        }
        return;
    }
    if let Some(typed) = line.strip_prefix("####") {
        let typed = typed.strip_prefix(' ').unwrap_or(typed);
        if typed.trim() != "<blank>" {
            conversation.push_line(Role::User, None, typed);
        }
        return;
    }
    if let Some(output) = line.strip_prefix('>') {
        if let Some(part) = tool(output.trim()) {
            conversation.push_part(Role::Assistant, None, part);
        }
        return;
    }
    if line.trim().is_empty() {
        // A blank line only matters inside a reply, between paragraphs.
        if let Some(last) = conversation.messages.last() {
            if last.role == Role::Assistant && matches!(last.parts.last(), Some(Part::Text { .. }))
            {
                conversation.push_line(Role::Assistant, None, "");
            }
        }
        return;
    }
    let continues_reply = conversation.messages.last().is_some_and(|last| {
        last.role == Role::Assistant && matches!(last.parts.last(), Some(Part::Text { .. }))
    });
    if continues_reply {
        conversation.push_line(Role::Assistant, None, line);
    } else {
        conversation.push_part(
            Role::Assistant,
            None,
            Part::Text {
                text: line.to_string(),
            },
        );
    }
}

/// The tool call an output line reports, if it is one worth keeping.
fn tool(output: &str) -> Option<Part> {
    let (name, summary) = [
        ("edit", "Applied edit to "),
        ("commit", "Commit "),
        ("run", "Running "),
    ]
    .into_iter()
    .find_map(|(name, prefix)| Some((name, output.strip_prefix(prefix)?)))?;
    Some(Part::Tool(ToolCall {
        id: None,
        name: name.to_string(),
        summary: one_line(summary, SUMMARY_MAX),
        failed: false,
    }))
}

fn local_time(text: &str) -> Option<DateTime<Utc>> {
    let naive = NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").ok()?;
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|at| at.with_timezone(&Utc))
}

/// A stable 64-bit FNV-1a hash, so ids do not depend on the Rust version.
fn fnv1a(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> (Vec<Conversation>, Conversation) {
        let path = Path::new("/work/app").join(HISTORY_FILE);
        let mut conversation = Conversation::new(Agent::Aider, file_session_id(&path));
        let mut finished = Vec::new();
        for line in text.lines() {
            apply_line(&mut conversation, line, &path, &mut finished);
        }
        (finished, conversation)
    }

    #[test]
    fn splits_sessions_and_keeps_turns() {
        let (finished, current) = read(
            "\n# aider chat started at 2026-01-02 10:00:00\n\n\
             > Add .aider* to .gitignore (Y)es/(N)o [Yes]: y\n\n\
             #### Add a greeting  \n#### in French\n\n\
             Here is the change.\n\nIt prints a greeting.\n\n\
             > Applied edit to hello.py  \n> Commit 1a2b3c4 feat: greet in French  \n\
             > Tokens: 1k sent, 100 received.\n\n\
             # aider chat started at 2026-01-03 09:30:00\n\n\
             #### <blank>\n",
        );
        assert_eq!(finished.len(), 1);
        let first = &finished[0];
        assert!(first.session_id.ends_with("-20260102T100000"));
        assert_eq!(first.project.as_deref(), Some(Path::new("/work/app")));
        assert_eq!(first.messages.len(), 2);
        assert_eq!(first.messages[0].text(), "Add a greeting\nin French");
        let reply = &first.messages[1];
        assert!(
            reply
                .text()
                .starts_with("Here is the change.\n\nIt prints a greeting.")
        );
        let tools: Vec<_> = reply.tool_calls().map(|c| c.name.as_str()).collect();
        assert_eq!(tools, ["edit", "commit"]);
        assert!(current.session_id.ends_with("-20260103T093000"));
        assert!(current.is_empty());
    }

    #[test]
    fn hashes_stably() {
        assert_eq!(fnv1a(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a("a"), 0xaf63_dc4c_8601_ec8c);
    }
}
