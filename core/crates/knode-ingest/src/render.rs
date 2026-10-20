//! Writing a conversation as Markdown.

use std::fmt::Write;

use crate::model::{Conversation, Message, Part, Role};
use crate::text::starts_with_tag;

/// The longest derived title, in characters.
const TITLE_MAX: usize = 80;

/// A title from the first thing the person wrote, or the agent and date when there is none.
pub fn derive_title(conversation: &Conversation) -> String {
    let first_line = conversation
        .messages
        .iter()
        .filter(|message| message.role == Role::User)
        .flat_map(|message| message.parts.iter())
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .flat_map(str::lines)
        .map(|line| {
            line.trim_start_matches(['#', '>', '-', '*', ' ', '\t'])
                .trim()
        })
        .find(|line| !line.is_empty() && !starts_with_tag(line) && !line.starts_with("```"));
    match first_line {
        Some(line) => shorten(line, TITLE_MAX),
        None => match conversation.started_at {
            Some(at) => format!(
                "{} session on {}",
                conversation.agent,
                at.format("%Y-%m-%d")
            ),
            None => format!("{} session", conversation.agent),
        },
    }
}

/// Collapses whitespace and cuts at a word boundary.
fn shorten(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        return collapsed;
    }
    let cut: String = collapsed.chars().take(max).collect();
    let cut = match cut.rfind(' ') {
        Some(space) if space > max / 2 => &cut[..space],
        _ => cut.as_str(),
    };
    format!("{}…", cut.trim_end_matches([',', '.', ';', ':', ' ']))
}

/// The conversation as a Markdown transcript: the title as a level 1 heading, then one level 2
/// section per turn. Headings inside messages are moved two levels down so they stay inside
/// their turn, tool calls become a list of one line each, and images a placeholder.
pub fn to_markdown(conversation: &Conversation) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# {}", conversation.title());
    for message in &conversation.messages {
        if message.is_empty() {
            continue;
        }
        let who = match message.role {
            Role::User => "User",
            Role::Assistant => conversation.agent.name(),
        };
        let _ = write!(out, "\n## {who}\n");
        write_message(&mut out, message);
    }
    out
}

fn write_message(out: &mut String, message: &Message) {
    let mut in_list = false;
    for part in &message.parts {
        match part {
            Part::Tool(call) => {
                if !in_list {
                    out.push('\n');
                    in_list = true;
                }
                let _ = write!(out, "- {}", code(&call.name));
                if !call.summary.is_empty() {
                    let _ = write!(out, ": {}", code(&call.summary));
                }
                if call.failed {
                    out.push_str(" (failed)");
                }
                out.push('\n');
            }
            Part::Text { text } => {
                in_list = false;
                let _ = write!(out, "\n{}\n", demote_headings(text.trim_end()));
            }
            Part::Image => {
                in_list = false;
                out.push_str("\n_[image]_\n");
            }
        }
    }
}

/// Inline code that holds any text, backticks included.
fn code(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat(longest + 1);
    if longest > 0 || text.starts_with(' ') || text.ends_with(' ') {
        format!("{fence} {text} {fence}")
    } else {
        format!("{fence}{text}{fence}")
    }
}

/// Moves ATX headings outside code blocks two levels down, up to level 6.
pub(crate) fn demote_headings(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut fence: Option<(char, usize)> = None;
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let indent = line.len() - line.trim_start_matches(' ').len();
        let body = &line[indent..];
        if indent <= 3 {
            if let Some((marker, len)) = fence_marker(body) {
                match fence {
                    None => fence = Some((marker, len)),
                    Some((open, open_len)) if open == marker && len >= open_len => fence = None,
                    _ => {}
                }
                out.push_str(line);
                continue;
            }
            if fence.is_none() {
                let hashes = body.chars().take_while(|&c| c == '#').count();
                let rest = &body[hashes..];
                if (1..=6).contains(&hashes) && (rest.is_empty() || rest.starts_with([' ', '\t'])) {
                    let level = (hashes + 2).min(6);
                    out.push_str(&"#".repeat(level));
                    out.push_str(rest);
                    continue;
                }
            }
        }
        out.push_str(line);
    }
    out
}

fn fence_marker(line: &str) -> Option<(char, usize)> {
    let marker = line.chars().next().filter(|&c| c == '`' || c == '~')?;
    let len = line.chars().take_while(|&c| c == marker).count();
    (len >= 3).then_some((marker, len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Agent, ToolCall};

    #[test]
    fn demotes_headings_but_not_code() {
        let text = "# Plan\nText\n```sh\n# a comment\n```\n###### Deep";
        assert_eq!(
            demote_headings(text),
            "### Plan\nText\n```sh\n# a comment\n```\n###### Deep"
        );
        assert_eq!(demote_headings("#hashtag"), "#hashtag");
    }

    #[test]
    fn wraps_backticks_in_code() {
        assert_eq!(code("ls"), "`ls`");
        assert_eq!(code("echo `date`"), "`` echo `date` ``");
    }

    #[test]
    fn derives_titles_from_the_first_user_line() {
        let mut conversation = Conversation::new(Agent::Codex, "id");
        assert_eq!(derive_title(&conversation), "Codex session");
        conversation.push_part(
            Role::User,
            None,
            Part::Text {
                text: "## Fix   the login bug\nmore".into(),
            },
        );
        assert_eq!(derive_title(&conversation), "Fix the login bug");
        let long = "word ".repeat(40);
        assert!(shorten(&long, TITLE_MAX).chars().count() <= TITLE_MAX + 1);
        assert!(shorten(&long, TITLE_MAX).ends_with('…'));
    }

    #[test]
    fn renders_turns_and_tool_lines() {
        let mut conversation = Conversation::new(Agent::ClaudeCode, "id");
        conversation.push_part(
            Role::User,
            None,
            Part::Text {
                text: "Run the tests".into(),
            },
        );
        conversation.push_part(
            Role::Assistant,
            None,
            Part::Tool(ToolCall {
                id: None,
                name: "Bash".into(),
                summary: "cargo test".into(),
                failed: true,
            }),
        );
        conversation.push_part(
            Role::Assistant,
            None,
            Part::Text {
                text: "One failed.".into(),
            },
        );
        assert_eq!(
            to_markdown(&conversation),
            "# Run the tests\n\n## User\n\nRun the tests\n\n## Claude Code\n\n- `Bash`: `cargo test` (failed)\n\nOne failed.\n"
        );
    }
}
