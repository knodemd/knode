//! Markdown helpers shared by the note importers: escaping text, titles, attachment names and
//! rewriting links.

use std::collections::BTreeSet;
use std::ops::Range;

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

/// The longest title written, in characters.
const MAX_TITLE: usize = 200;

/// A title the format accepts: `[`, `]` and `|` (which `[[links]]` use) replaced, whitespace as
/// single spaces, at most 200 characters, `Untitled` when empty.
pub(crate) fn title(raw: &str) -> String {
    let title = crate::names::title_from_stem(raw);
    let mut title: String = title.chars().take(MAX_TITLE).collect();
    title = title.trim().to_string();
    if title.is_empty() {
        "Untitled".to_string()
    } else {
        title
    }
}

/// An attachment name as the format keeps it: control characters dropped, `/ \ [ ] | #` as
/// `-`, trimmed; `attachment` when nothing is left.
pub(crate) fn attachment_name(raw: &str) -> String {
    let name: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| match c {
            '/' | '\\' | '[' | ']' | '|' | '#' => '-',
            c => c,
        })
        .collect();
    let name = name.trim();
    if name.is_empty() || name.chars().all(|c| c == '.' || c == '-') {
        "attachment".to_string()
    } else {
        name.chars().take(200).collect()
    }
}

/// The names of the attachments of one note, each one used once: `photo.png`, `photo 2.png`.
#[derive(Debug, Default)]
pub(crate) struct Names(BTreeSet<String>);

impl Names {
    /// A free name for a file called `raw`.
    pub(crate) fn take(&mut self, raw: &str) -> String {
        let name = attachment_name(raw);
        let mut chosen = name.clone();
        let mut n = 2;
        while !self.0.insert(chosen.to_lowercase()) {
            chosen = crate::attachments::numbered(&name, n);
            n += 1;
        }
        chosen
    }
}

/// Whether a file name is an image Markdown can show.
pub(crate) fn is_image(name: &str) -> bool {
    let lower = name.to_lowercase();
    [
        ".png", ".jpg", ".jpeg", ".gif", ".webp", ".heic", ".heif", ".svg", ".tif", ".tiff",
        ".bmp", ".avif",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

/// How an attachment is written in a body: `![[photo.png]]` for an image, else `[[file.pdf]]`.
pub(crate) fn embed(name: &str) -> String {
    if is_image(name) {
        format!("![[{name}]]")
    } else {
        format!("[[{name}]]")
    }
}

/// A link to the node titled `title`, showing `text` when it is not the title.
pub(crate) fn wiki(title: &str, text: &str) -> String {
    let text: String = text
        .chars()
        .map(|c| match c {
            '[' => '(',
            ']' => ')',
            '|' => '-',
            c => c,
        })
        .collect();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() || text == title {
        format!("[[{title}]]")
    } else {
        format!("[[{title}|{text}]]")
    }
}

/// Plain text written so Markdown shows it as it is: `\ * _ ` [ ] < >` escaped, and so are
/// `#`, `>`, `-`, `+` and `1.` at the start of a line, which would start a block.
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut line_start = true;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if line_start {
            // Leading spaces stay; what follows them is checked.
            if c == ' ' || c == '\t' {
                out.push(c);
                i += 1;
                continue;
            }
            let next = chars.get(i + 1).copied();
            let starts_block = match c {
                '#' | '>' => true,
                '-' | '+' => matches!(next, Some(' ' | '\t') | None),
                '0'..='9' => {
                    let mut j = i;
                    while j < chars.len() && chars[j].is_ascii_digit() {
                        j += 1;
                    }
                    matches!(chars.get(j), Some('.' | ')'))
                        && matches!(chars.get(j + 1), Some(' ' | '\t') | None)
                }
                _ => false,
            };
            if starts_block {
                if c.is_ascii_digit() {
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        out.push(chars[i]);
                        i += 1;
                    }
                    out.push('\\');
                    out.push(chars[i]);
                    i += 1;
                    line_start = false;
                    continue;
                }
                out.push('\\');
            }
            line_start = false;
        }
        if matches!(c, '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '>') {
            out.push('\\');
        }
        out.push(c);
        if c == '\n' {
            line_start = true;
        }
        i += 1;
    }
    out
}

/// Wraps `text` in `marker` (`**`, `*`, `~~`), keeping the spaces around it outside, as
/// Markdown needs.
pub(crate) fn wrap(text: &str, marker: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return text.to_string();
    }
    let start = text.len() - text.trim_start().len();
    let end = start + trimmed.len();
    format!(
        "{}{marker}{trimmed}{marker}{}",
        &text[..start],
        &text[end..]
    )
}

/// Runs of three or more line breaks as two, no space at the end of lines, and a body that
/// ends with one line break (or is empty).
pub(crate) fn tidy(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut blank = 0;
    let mut in_fence = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        let line = if in_fence { line } else { line.trim_end() };
        if line.trim().is_empty() && !in_fence {
            blank += 1;
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
            if blank > 0 {
                out.push('\n');
            }
        }
        blank = 0;
        out.push_str(line);
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// An inline link or image of a body, as [`rewrite_links`] passes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkRef {
    /// `![...](...)` rather than `[...](...)`.
    pub image: bool,
    /// The destination, as Markdown reads it (backslash escapes and `<>` removed).
    pub dest: String,
    /// The text (or image description), plain.
    pub text: String,
}

/// Replaces the inline links and images of `body` for which `replace` gives new Markdown. A link
/// inside a link (an image in a link) is only replaced when the outer one is not.
pub(crate) fn rewrite_links(
    body: &str,
    mut replace: impl FnMut(&LinkRef) -> Option<String>,
) -> String {
    let mut found: Vec<(Range<usize>, LinkRef)> = Vec::new();
    let mut open: Vec<(Range<usize>, LinkRef)> = Vec::new();
    for (event, range) in Parser::new_ext(body, knode_md::options()).into_offset_iter() {
        match event {
            Event::Start(Tag::Link { dest_url, .. }) => open.push((
                range,
                LinkRef {
                    image: false,
                    dest: dest_url.into_string(),
                    text: String::new(),
                },
            )),
            Event::Start(Tag::Image { dest_url, .. }) => open.push((
                range,
                LinkRef {
                    image: true,
                    dest: dest_url.into_string(),
                    text: String::new(),
                },
            )),
            Event::Text(t) | Event::Code(t) => {
                for (_, link) in open.iter_mut() {
                    link.text.push_str(&t);
                }
            }
            Event::End(TagEnd::Link | TagEnd::Image) => {
                found.extend(open.pop());
            }
            _ => {}
        }
    }
    // Outer links come first (they were opened first); skip any range inside a replaced one.
    found.sort_by_key(|(r, _)| (r.start, std::cmp::Reverse(r.end)));
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    for (range, link) in found {
        if edits.last().is_some_and(|(r, _)| range.start < r.end) {
            continue;
        }
        // Only links written inline, `[text](dest)`: reference links end with `]`.
        if !body[range.clone()].ends_with(')') {
            continue;
        }
        if let Some(new) = replace(&link) {
            edits.push((range, new));
        }
    }
    let mut out = body.to_string();
    for (range, new) in edits.into_iter().rev() {
        out.replace_range(range, &new);
    }
    out
}

/// A percent-encoded path decoded, as exports write file names in links.
pub(crate) fn decode_path(dest: &str) -> String {
    percent_encoding::percent_decode_str(dest)
        .decode_utf8_lossy()
        .into_owned()
}

/// Whether a link destination is a web address or another scheme, not a file.
pub(crate) fn is_url(dest: &str) -> bool {
    match dest.split_once(':') {
        Some((scheme, _)) => {
            scheme.len() > 1
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        }
        None => false,
    }
}

/// A tag as the format keeps it, from an app's tag: spaces as `-`, then cleaned.
pub(crate) fn tag(raw: &str) -> Option<String> {
    let joined = raw.split_whitespace().collect::<Vec<_>>().join("-");
    crate::tags::clean(&joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_and_names() {
        assert_eq!(title("  A [draft] | v2 "), "A (draft) - v2");
        assert_eq!(title(""), "Untitled");
        assert_eq!(attachment_name("a/b#c.png"), "a-b-c.png");
        let mut names = Names::default();
        assert_eq!(names.take("photo.png"), "photo.png");
        assert_eq!(names.take("Photo.png"), "Photo 2.png");
        assert_eq!(embed("photo.png"), "![[photo.png]]");
        assert_eq!(embed("plan.pdf"), "[[plan.pdf]]");
        assert_eq!(wiki("Plan", "Plan"), "[[Plan]]");
        assert_eq!(wiki("Plan", "the [plan]"), "[[Plan|the (plan)]]");
    }

    #[test]
    fn escapes_text() {
        assert_eq!(escape("a *b* [c]"), "a \\*b\\* \\[c\\]");
        assert_eq!(escape("# not a heading"), "\\# not a heading");
        assert_eq!(
            escape("- not a list\n1. nor this"),
            "\\- not a list\n1\\. nor this"
        );
        assert_eq!(escape("a - b 1. c #d"), "a - b 1. c #d");
        assert_eq!(wrap(" bold ", "**"), " **bold** ");
    }

    #[test]
    fn tidies_bodies() {
        assert_eq!(tidy("\n\na  \n\n\n\nb\n\n"), "a\n\nb\n");
        assert_eq!(tidy("```\nx\n\n\ny\n```"), "```\nx\n\n\ny\n```\n");
        assert_eq!(tidy("  \n"), "");
    }

    #[test]
    fn rewrites_inline_links() {
        let body = "See [Plan](Plan%20abc.md) and ![](img.png), [web](https://x.y), [r][1].\n\n[1]: a.md\n";
        let out = rewrite_links(body, |link| {
            (!is_url(&link.dest)).then(|| format!("<{}:{}:{}>", link.image, link.dest, link.text))
        });
        assert_eq!(
            out,
            "See <false:Plan%20abc.md:Plan> and <true:img.png:>, [web](https://x.y), [r][1].\n\n[1]: a.md\n"
        );
        let nested = rewrite_links("[![a](i.png)](o.md)", |link| {
            Some(format!("<{}>", link.dest))
        });
        assert_eq!(nested, "<o.md>");
        assert_eq!(decode_path("My%20Page.md"), "My Page.md");
        assert!(is_url("https://x.y") && is_url("evernote:///view") && !is_url("a b.png"));
        assert_eq!(tag(" Multi Word ").as_deref(), Some("multi-word"));
    }
}
