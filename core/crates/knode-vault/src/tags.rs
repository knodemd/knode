//! Tags: cleaning them as the format stores them, and finding inline `#tags` in a body.

use std::collections::BTreeSet;

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

/// A tag as the format stores it: trimmed, lower case, without a leading `#`, one word of at most
/// 64 characters. `None` when the text is not a valid tag.
pub(crate) fn clean(tag: &str) -> Option<String> {
    let tag = tag.trim().trim_start_matches('#').trim().to_lowercase();
    let valid =
        !tag.is_empty() && !tag.chars().any(char::is_whitespace) && tag.chars().count() <= 64;
    valid.then_some(tag)
}

/// The inline `#tags` of a Markdown body, as Obsidian reads them: `#` at the start of a word,
/// followed by letters, digits, `_`, `-` or `/`, with at least one character that is not a
/// digit. Tags in code, links and HTML are not tags. Returned cleaned.
pub(crate) fn inline(body: &str) -> BTreeSet<String> {
    let mut text = String::new();
    let mut in_link = 0usize;
    let mut in_code = false;
    for event in Parser::new_ext(body, knode_md::options()) {
        match event {
            Event::Text(t) if in_link == 0 && !in_code => text.push_str(&t),
            Event::Start(Tag::CodeBlock(_)) => in_code = true,
            Event::End(TagEnd::CodeBlock) => {
                in_code = false;
                text.push('\n');
            }
            Event::Start(Tag::Link { .. } | Tag::Image { .. }) => {
                in_link += 1;
                text.push(' ');
            }
            Event::End(TagEnd::Link | TagEnd::Image) => {
                in_link = in_link.saturating_sub(1);
                text.push(' ');
            }
            Event::Start(
                Tag::Emphasis
                | Tag::Strong
                | Tag::Strikethrough
                | Tag::Superscript
                | Tag::Subscript,
            )
            | Event::End(
                TagEnd::Emphasis
                | TagEnd::Strong
                | TagEnd::Strikethrough
                | TagEnd::Superscript
                | TagEnd::Subscript,
            ) => {}
            _ => text.push('\n'),
        }
    }
    let mut out = BTreeSet::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let at_word_start = i == 0 || chars[i - 1].is_whitespace();
        if chars[i] == '#' && at_word_start {
            let start = i + 1;
            let mut end = start;
            while end < chars.len()
                && (chars[end].is_alphanumeric() || matches!(chars[end], '_' | '-' | '/'))
            {
                end += 1;
            }
            let word: String = chars[start..end].iter().collect();
            if word.chars().any(|c| !c.is_ascii_digit()) {
                out.extend(clean(&word));
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(body: &str) -> Vec<String> {
        inline(body).into_iter().collect()
    }

    #[test]
    fn finds_inline_tags() {
        assert_eq!(
            tags("An #Idea and #project/active, not a#tag nor #123.\n\n#start"),
            ["idea", "project/active", "start"]
        );
    }

    #[test]
    fn skips_headings_code_and_links() {
        assert!(
            tags("# Heading\n\n`#code`\n\n```\n#block\n```\n\n[[Note#heading]] [[#Own]]")
                .is_empty()
        );
        assert_eq!(tags("**#bold** and *#em*"), ["bold", "em"]);
    }

    #[test]
    fn cleans_tags() {
        assert_eq!(clean(" #Work ").as_deref(), Some("work"));
        assert_eq!(clean("two words"), None);
        assert_eq!(clean("#"), None);
    }
}
