//! A chat as Markdown: one `## YYYY-MM-DD` section per day, and in it one paragraph per
//! message, `**Sender** (HH:MM): text`, with the files sent after the text.

use super::model::ChatMessage;

/// Extensions shown as an image (`![[name]]`) rather than linked (`[[name]]`).
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "heic", "heif", "bmp", "svg", "avif",
];

/// How a file sent in a message is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Shown {
    /// Attached to the chat's node under this name.
    Attached(String),
    /// In the export, but it could not be attached, and why.
    NotAttached { name: String, reason: String },
    /// Named by the chat, but not in the export.
    Missing(String),
    /// Left out of the export: what it was.
    Omitted(String),
}

/// The heading of a day section, without `## `.
pub(crate) fn day_heading(message: &ChatMessage) -> String {
    message.time.format("%Y-%m-%d").to_string()
}

/// The day of the last day section in `body`, if any.
fn last_day(body: &str) -> Option<&str> {
    let mut fence = None;
    let mut day = None;
    for line in body.lines() {
        match fence {
            Some(open) => {
                if closes(line, open) {
                    fence = None;
                }
            }
            None => {
                if let Some(open) = opens(line) {
                    fence = Some(open);
                } else if let Some(heading) = line.strip_prefix("## ") {
                    day = Some(heading.trim());
                }
            }
        }
    }
    day
}

/// `body` with `messages` (and how their files are shown) added at the end: a message of the
/// day `body` ends with goes under it, others under new day sections.
pub(crate) fn append(body: &str, messages: &[(&ChatMessage, Vec<Shown>)]) -> String {
    let mut out = body.trim_end().to_string();
    let mut day = last_day(body).map(str::to_string);
    for (message, shown) in messages {
        let heading = day_heading(message);
        if day.as_deref() != Some(heading.as_str()) {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str("## ");
            out.push_str(&heading);
            day = Some(heading);
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&message_markdown(message, shown));
    }
    out.push('\n');
    out
}

/// One message as Markdown.
pub(crate) fn message_markdown(message: &ChatMessage, shown: &[Shown]) -> String {
    let time = message.time.format("%H:%M");
    let mut lines: Vec<String> = escape_blocks(&message.text)
        .lines()
        .map(str::to_string)
        .collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    // A blank line inside a message would end its paragraph: keep the lines together.
    let mut lines: Vec<String> = lines.into_iter().filter(|l| !l.trim().is_empty()).collect();
    lines.extend(shown.iter().map(shown_markdown));
    lines.extend(message.notes.iter().cloned());

    let Some(sender) = &message.sender else {
        let text = lines.join(" ");
        return format!("*({time}) {}*", text.trim());
    };
    let mut head = format!("**{}** ({time}", escape_name(sender));
    if let Some(reply) = &message.reply_to {
        head.push_str(", replying to ");
        head.push_str(reply);
    }
    head.push(')');
    match lines.split_first() {
        Some((first, rest)) => {
            let mut out = format!("{head}: {first}");
            for line in rest {
                out.push('\n');
                out.push_str(line);
            }
            out
        }
        None => head,
    }
}

fn shown_markdown(shown: &Shown) -> String {
    match shown {
        Shown::Attached(name) if is_image(name) => format!("![[{name}]]"),
        Shown::Attached(name) => format!("[[{name}]]"),
        Shown::NotAttached { name, reason } => {
            format!("*[not attached: {}, {reason}]*", escape_name(name))
        }
        Shown::Missing(name) => format!("*[missing from the export: {}]*", escape_name(name)),
        Shown::Omitted(what) => format!("*[{} left out of the export]*", escape_name(what)),
    }
}

/// Whether a file name is an image's.
pub(crate) fn is_image(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| IMAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// A name shown inline: Markdown emphasis and link characters escaped.
pub(crate) fn escape_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '*' | '_' | '[' | ']' | '`' | '<' | '>' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Plain text from a chat, ready to go into Markdown: `[[` and `]]`, which would link to
/// nodes, are escaped, and so is the start of every line Markdown would read as a block of its
/// own (a heading, a fence, a rule or the underline of a heading). The text keeps its other
/// characters as written.
pub(crate) fn escape_text(text: &str) -> String {
    let text = text.replace("[[", "\\[\\[").replace("]]", "\\]\\]");
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent <= 3 && starts_block(&line[indent..]) {
            out.push_str(&line[..indent]);
            out.push('\\');
            out.push_str(&line[indent..]);
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Whether a line, without its indent, starts a block: a heading, a fence, or a line of only
/// `-`, `=`, `*` or `_` (a rule, or the underline that makes the line before a heading).
fn starts_block(line: &str) -> bool {
    let trimmed = line.trim_end();
    is_heading(trimmed)
        || trimmed.starts_with("```")
        || trimmed.starts_with("~~~")
        || (!trimmed.is_empty()
            && ["-", "=", "*", "_"].iter().any(|c| {
                trimmed.starts_with(c) && trimmed.chars().all(|x| x == ' ' || c.starts_with(x))
            }))
}

/// Whether a line is an ATX heading: one to six `#`, then a space or nothing.
fn is_heading(line: &str) -> bool {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes)
        && line[hashes..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace)
}

/// The fence a line opens: its character and length. A fence of backticks cannot have one in
/// its info string, so ```` ```code``` ```` is not one.
fn opens(line: &str) -> Option<(char, usize)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let line = &line[indent..];
    let c = line.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let n = line.chars().take_while(|x| *x == c).count();
    (n >= 3 && (c == '~' || !line[n..].contains('`'))).then_some((c, n))
}

/// Whether a line closes the fence `open`.
fn closes(line: &str, (c, n): (char, usize)) -> bool {
    let trimmed = line.trim();
    let run = trimmed.chars().take_while(|x| *x == c).count();
    run >= n && trimmed.chars().all(|x| x == c) && line.len() - line.trim_start().len() <= 3
}

/// The lines of a message escaped so that none starts a section or a block outside the fenced
/// code an importer wrote, and a fence left open closed, so a message never reaches the next.
fn escape_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fence: Option<(char, usize)> = None;
    for line in text.lines() {
        match fence {
            Some(open) => {
                if closes(line, open) {
                    fence = None;
                }
            }
            None => {
                if let Some(open) = opens(line) {
                    fence = Some(open);
                } else {
                    let indent = line.len() - line.trim_start_matches(' ').len();
                    if indent <= 3 && starts_block(&line[indent..]) {
                        out.push('\\');
                    }
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    if let Some((c, n)) = fence {
        out.extend(std::iter::repeat_n(c, n));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    fn message(day: u32, hour: u32, sender: Option<&str>, text: &str) -> ChatMessage {
        let time = NaiveDate::from_ymd_opt(2026, 3, day)
            .unwrap()
            .and_hms_opt(hour, 5, 0)
            .unwrap();
        ChatMessage::new("k", "m", time, sender.map(str::to_string), text)
    }

    #[test]
    fn groups_messages_by_day() {
        let a = message(4, 9, Some("Ada"), "Hello");
        let b = message(4, 10, Some("Grace"), "Hi\nthere");
        let c = message(5, 8, None, "Ada added Linus");
        let body = append("", &[(&a, vec![]), (&b, vec![]), (&c, vec![])]);
        assert_eq!(
            body,
            "## 2026-03-04\n\n**Ada** (09:05): Hello\n\n**Grace** (10:05): Hi\nthere\n\n\
             ## 2026-03-05\n\n*(08:05) Ada added Linus*\n"
        );
        // More of the same day goes under its section.
        let d = message(5, 9, Some("Linus"), "Morning");
        let more = append(&body, &[(&d, vec![])]);
        assert!(more.ends_with("Ada added Linus*\n\n**Linus** (09:05): Morning\n"));
        assert_eq!(more.matches("## 2026-03-05").count(), 1);
    }

    #[test]
    fn shows_files_and_replies() {
        let mut m = message(4, 9, Some("Ada"), "");
        m.reply_to = Some("Grace at 09:00: \"Lunch?\"".into());
        let shown = vec![
            Shown::Attached("photo.jpg".into()),
            Shown::Attached("plan.pdf".into()),
            Shown::Missing("voice.opus".into()),
            Shown::Omitted("media".into()),
        ];
        assert_eq!(
            message_markdown(&m, &shown),
            "**Ada** (09:05, replying to Grace at 09:00: \"Lunch?\"): ![[photo.jpg]]\n[[plan.pdf]]\n\
             *[missing from the export: voice.opus]*\n*[media left out of the export]*"
        );
    }

    #[test]
    fn a_message_never_starts_a_section() {
        let m = message(
            4,
            9,
            Some("Ada_1"),
            "## not a day\nline\n---\n\n```\n# code\n```",
        );
        assert_eq!(
            message_markdown(&m, &[]),
            "**Ada\\_1** (09:05): \\## not a day\nline\n\\---\n```\n# code\n```"
        );
        assert_eq!(escape_text("see [[Plan]]"), "see \\[\\[Plan\\]\\]");
    }

    #[test]
    fn text_never_starts_a_block() {
        assert_eq!(
            escape_text("```mono```\n## 2026-03-20\n---\n***\n  # h\n#tag\n=\n    # code"),
            "\\```mono```\n\\## 2026-03-20\n\\---\n\\***\n  \\# h\n#tag\n\\=\n    # code"
        );
        // Inline code is not a fence: the day after it is still escaped.
        let m = message(
            4,
            9,
            Some("Ada"),
            &escape_text("```mono```\n## 2026-03-20\nok"),
        );
        let body = append("", &[(&m, vec![])]);
        assert!(!body.contains("\n## 2026-03-20"), "{body}");
        assert_eq!(last_day(&body), Some("2026-03-04"));
        // A fence left open is closed with the message.
        let m = message(4, 9, Some("Ada"), "see\n```\ncode");
        let body = append("", &[(&m, vec![])]);
        assert!(body.ends_with("```\ncode\n```\n"), "{body}");
    }

    #[test]
    fn the_last_day_is_outside_code() {
        let body = "## 2026-03-04\n\n**A** (09:00): x\n```\n## 2026-03-09\n```\n";
        assert_eq!(last_day(body), Some("2026-03-04"));
        let later = message(4, 10, Some("B"), "y");
        let more = append(body, &[(&later, vec![])]);
        assert_eq!(more.matches("## 2026-03-04").count(), 1, "{more}");
    }
}
