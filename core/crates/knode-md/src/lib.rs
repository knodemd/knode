//! Read the Markdown of `.knode` nodes.
//!
//! Every node body in a `.knode` space is Markdown: CommonMark with GitHub Flavored Markdown
//! (tables, strikethrough, task lists, footnotes) and `[[links]]`. This crate reads that dialect
//! once, when a node is written, so that search and the graph never parse Markdown:
//!
//! - [`extract`] returns the plain text to index and the `[[links]]` the body contains;
//! - [`blocks`] returns the lines of that plain text, marking the headings, for chunking;
//! - [`rename_links`] rewrites `[[Old title]]` into `[[New title]]` when a node is renamed.
//!
//! ```
//! let out = knode_md::extract("# Plan\n\nShip **v0.1**, see [[Roadmap|the roadmap]].");
//! assert_eq!(out.plain, "Plan\nShip v0.1, see the roadmap.");
//! assert_eq!(out.links[0].target, "Roadmap");
//! assert_eq!(out.links[0].label.as_deref(), Some("the roadmap"));
//! ```

use pulldown_cmark::{CowStr, Event, LinkType, Options, Parser, Tag, TagEnd};

/// The Markdown options of the `.knode` dialect: CommonMark plus GFM tables, strikethrough,
/// task lists, footnotes and `[[links]]`.
pub fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_WIKILINKS
}

/// A `[[link]]` found in a node body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiLink {
    /// What the link points at: a node title or a node id, without any `#heading` part.
    pub target: String,
    /// The part after `#`, as in `[[target#heading]]`, if any. A title may itself contain `#`:
    /// resolve [`destination`](Self::destination) as a whole title first, then `target`.
    pub heading: Option<String>,
    /// Everything before `|` as written, trimmed: `target#heading`.
    pub destination: String,
    /// The text after `|`, as in `[[target|label]]`, if any.
    pub label: Option<String>,
    /// Whether it was written as an embed, `![[target]]`.
    pub embed: bool,
}

/// What [`extract`] reads from a node body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extracted {
    /// The text without Markdown syntax, one line per block, for full-text search.
    pub plain: String,
    /// Every `[[link]]`, in order of appearance, duplicates included.
    pub links: Vec<WikiLink>,
}

/// Reads a node body: its plain text and its `[[links]]`.
///
/// The plain text keeps the words a person reads: headings, paragraphs, list items, table
/// cells, code, link text and image descriptions. Markup, raw HTML and URLs are dropped.
/// Blocks are separated by a newline; runs of whitespace inside a block become one space.
pub fn extract(markdown: &str) -> Extracted {
    read(markdown, None)
}

/// A block of a node body: one line of its plain text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// The text of the block, a line of the plain text.
    pub text: String,
    /// The level of the heading, 1 to 6, when the block is a heading.
    pub heading: Option<u8>,
}

/// Reads the blocks of a node body: the lines of its plain text, in order, with the level of
/// each heading. Joined with `\n`, their texts are exactly the plain text of [`extract`].
///
/// ```
/// let blocks = knode_md::blocks("# Plan\n\nShip **v0.1**.\n\n## Q4\n\n- one");
/// let lines: Vec<_> = blocks.iter().map(|b| (b.text.as_str(), b.heading)).collect();
/// assert_eq!(lines, [("Plan", Some(1)), ("Ship v0.1.", None), ("Q4", Some(2)), ("one", None)]);
/// ```
pub fn blocks(markdown: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    read(markdown, Some(&mut blocks));
    blocks
}

fn read(markdown: &str, mut blocks: Option<&mut Vec<Block>>) -> Extracted {
    let mut out = Extracted::default();
    let mut block = String::new();
    // The level of the heading being read, if inside one.
    let mut heading: Option<u8> = None;
    // The `[[link]]` being read, if inside one.
    let mut wiki: Option<WikiLink> = None;

    let mut flush = |block: &mut String, plain: &mut String, heading: Option<u8>| {
        let line = block.split_whitespace().collect::<Vec<_>>().join(" ");
        if !line.is_empty() {
            if !plain.is_empty() {
                plain.push('\n');
            }
            plain.push_str(&line);
            if let Some(blocks) = blocks.as_deref_mut() {
                blocks.push(Block {
                    text: line,
                    heading,
                });
            }
        }
        block.clear();
    };

    for event in Parser::new_ext(markdown, options()) {
        match event {
            Event::Start(Tag::Link {
                link_type: LinkType::WikiLink { .. },
                dest_url,
                ..
            })
            | Event::Start(Tag::Image {
                link_type: LinkType::WikiLink { .. },
                dest_url,
                ..
            }) => {
                let (target, heading) = split_target(&dest_url);
                wiki = Some(WikiLink {
                    target,
                    heading,
                    destination: dest_url.trim().to_string(),
                    label: None,
                    embed: false,
                });
            }
            Event::End(TagEnd::Link) | Event::End(TagEnd::Image) if wiki.is_some() => {
                if let Some(link) = wiki.take() {
                    out.links.push(link);
                }
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some(link) = wiki.as_mut() {
                    let label = link.label.get_or_insert_with(String::new);
                    label.push_str(&text);
                }
                block.push_str(&text);
            }
            Event::SoftBreak | Event::HardBreak => block.push(' '),
            Event::InlineMath(math) | Event::DisplayMath(math) => block.push_str(&math),
            Event::FootnoteReference(_)
            | Event::TaskListMarker(_)
            | Event::Rule
            | Event::Html(_)
            | Event::InlineHtml(_) => {}
            Event::Start(Tag::Heading { level, .. }) => heading = Some(level as u8),
            Event::Start(_) => {}
            Event::End(end) => match end {
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::CodeBlock
                | TagEnd::Item
                | TagEnd::TableCell
                | TagEnd::FootnoteDefinition
                | TagEnd::DefinitionListTitle
                | TagEnd::DefinitionListDefinition => {
                    if matches!(end, TagEnd::TableCell) {
                        block.push(' ');
                    } else {
                        flush(&mut block, &mut out.plain, heading.take());
                    }
                }
                TagEnd::TableRow | TagEnd::TableHead => flush(&mut block, &mut out.plain, None),
                _ => {}
            },
        }
    }
    flush(&mut block, &mut out.plain, None);

    // A label equal to the target is not a label: `[[Roadmap]]` has none.
    for link in &mut out.links {
        if link.label.as_deref() == Some(link.target.as_str()) {
            link.label = None;
        }
    }
    mark_embeds(markdown, &mut out.links);
    out
}

/// Returns only the plain text of a node body. See [`extract`].
pub fn plain_text(markdown: &str) -> String {
    extract(markdown).plain
}

/// Returns only the `[[links]]` of a node body. See [`extract`].
pub fn wiki_links(markdown: &str) -> Vec<WikiLink> {
    extract(markdown).links
}

/// Rewrites every `[[old]]` link in `markdown` to point at `new`, keeping labels, `#heading`
/// parts and embeds: `[[Old]]` becomes `[[New]]` and `[[Old#Plan|see]]` becomes
/// `[[New#Plan|see]]`. Titles are compared by [`title_key`]. When `old` itself contains `#`,
/// `[[old]]` as a whole is rewritten. Text inside code is never touched.
///
/// ```
/// let md = "See [[Old]] and `[[Old]]`.";
/// assert_eq!(knode_md::rename_links(md, "old", "New"), "See [[New]] and `[[Old]]`.");
/// ```
pub fn rename_links(markdown: &str, old: &str, new: &str) -> String {
    let mut edits: Vec<(usize, usize)> = Vec::new();
    for (event, range) in Parser::new_ext(markdown, options()).into_offset_iter() {
        let dest = match &event {
            Event::Start(Tag::Link {
                link_type: LinkType::WikiLink { .. },
                dest_url,
                ..
            })
            | Event::Start(Tag::Image {
                link_type: LinkType::WikiLink { .. },
                dest_url,
                ..
            }) => dest_url,
            _ => continue,
        };
        // The range covers `[[...]]`, possibly with a leading `!`; the destination follows `[[`
        // exactly as written.
        let Some(open) = markdown[range.clone()].find("[[") else {
            continue;
        };
        let start = range.start + open + 2;
        if markdown.get(start..start + dest.len()) != Some(dest.as_ref()) {
            continue;
        }
        // Prefer the whole destination as a title (titles may contain `#`), then the part
        // before `#`.
        let whole = dest.as_ref();
        let candidates = [Some(whole), whole.split_once('#').map(|(t, _)| t)];
        for part in candidates.into_iter().flatten() {
            if title_key(part) == title_key(old) {
                let leading = part.len() - part.trim_start().len();
                edits.push((start + leading, start + leading + part.trim().len()));
                break;
            }
        }
    }
    let mut out = String::with_capacity(markdown.len());
    let mut last = 0;
    for (start, end) in edits {
        out.push_str(&markdown[last..start]);
        out.push_str(new);
        last = end;
    }
    out.push_str(&markdown[last..]);
    out
}

/// Splits a `[[link]]` destination into a target and a `#heading`, both trimmed.
fn split_target(dest: &CowStr<'_>) -> (String, Option<String>) {
    match dest.split_once('#') {
        Some((target, heading)) => (target.trim().to_string(), Some(heading.trim().to_string())),
        None => (dest.trim().to_string(), None),
    }
}

/// Marks the links written as `![[...]]` as embeds.
fn mark_embeds(markdown: &str, links: &mut [WikiLink]) {
    let mut embeds = Parser::new_ext(markdown, options()).filter_map(|e| match e {
        Event::Start(Tag::Link {
            link_type: LinkType::WikiLink { .. },
            ..
        }) => Some(false),
        Event::Start(Tag::Image {
            link_type: LinkType::WikiLink { .. },
            ..
        }) => Some(true),
        _ => None,
    });
    for link in links {
        link.embed = embeds.next().unwrap_or(false);
    }
}

/// The key two titles are compared by when a `[[link]]` is resolved: lower case (Unicode),
/// trimmed, with runs of whitespace as one space. `"  Café  de Ideas"` and `"café de ideas"` have
/// the same key.
///
/// ```
/// assert_eq!(knode_md::title_key("  Café  de IDEAS "), "café de ideas");
/// ```
pub fn title_key(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets(md: &str) -> Vec<String> {
        wiki_links(md).into_iter().map(|l| l.target).collect()
    }

    #[test]
    fn plain_text_drops_markup() {
        let md = "# Title\n\nSome *emphasis* and **strong** and `code`.\n\n- one\n- [x] two\n\n> quoted\n> text\n";
        assert_eq!(
            plain_text(md),
            "Title\nSome emphasis and strong and code.\none\ntwo\nquoted text"
        );
    }

    #[test]
    fn plain_text_keeps_link_text_and_drops_urls_and_html() {
        let md = "Read [the docs](https://knode.md) <b>now</b> ![a logo](logo.png)";
        assert_eq!(plain_text(md), "Read the docs now a logo");
    }

    #[test]
    fn plain_text_reads_code_blocks_and_tables() {
        let md = "```rust\nfn main() {}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        assert_eq!(plain_text(md), "fn main() {}\na b\n1 2");
    }

    #[test]
    fn blocks_are_the_lines_of_the_plain_text_with_headings() {
        let md = "Intro\n\n# Plan\n\n> ## Quoted *head*\n\nText\nwraps\n\n#\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n###### Six\n";
        let blocks = blocks(md);
        let lines: Vec<_> = blocks
            .iter()
            .map(|b| (b.text.as_str(), b.heading))
            .collect();
        assert_eq!(
            lines,
            [
                ("Intro", None),
                ("Plan", Some(1)),
                ("Quoted head", Some(2)),
                ("Text wraps", None),
                ("a b", None),
                ("1 2", None),
                ("Six", Some(6)),
            ]
        );
        let joined: Vec<_> = blocks.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(joined.join("\n"), plain_text(md));
        assert!(super::blocks("").is_empty());
    }

    #[test]
    fn plain_text_of_empty_body() {
        assert_eq!(plain_text(""), "");
        assert!(wiki_links("").is_empty());
    }

    #[test]
    fn finds_wiki_links() {
        let md = "See [[Roadmap]], [[Plan|the plan]] and [[Spec#Schema]].";
        let links = wiki_links(md);
        assert_eq!(targets(md), ["Roadmap", "Plan", "Spec"]);
        assert_eq!(links[0].label, None);
        assert_eq!(links[1].label.as_deref(), Some("the plan"));
        assert_eq!(plain_text(md), "See Roadmap, the plan and Spec#Schema.");
    }

    #[test]
    fn finds_embeds() {
        let links = wiki_links("![[Diagram]] and [[Note]]");
        assert!(links[0].embed);
        assert!(!links[1].embed);
    }

    #[test]
    fn ignores_links_in_code() {
        assert!(targets("`[[Not a link]]`\n\n```\n[[Nor this]]\n```").is_empty());
    }

    #[test]
    fn ignores_empty_links() {
        assert!(targets("[[]] and [[|label]]").is_empty());
    }

    #[test]
    fn keeps_unicode_targets() {
        assert_eq!(targets("[[Café de ideas]]"), ["Café de ideas"]);
    }

    #[test]
    fn renames_links() {
        let md = "[[Old]] and [[old|label]] and ![[Old#Part]] and [[Other]] and `[[Old]]`";
        assert_eq!(
            rename_links(md, "Old", "New name"),
            "[[New name]] and [[New name|label]] and ![[New name#Part]] and [[Other]] and `[[Old]]`"
        );
    }

    #[test]
    fn renames_unicode_case_insensitively() {
        assert_eq!(rename_links("[[ÁRBOL]]", "árbol", "Tree"), "[[Tree]]");
    }

    #[test]
    fn renames_titles_with_a_hash() {
        assert_eq!(
            rename_links("[[Issue #3]] and [[Issue]]", "Issue #3", "Spec"),
            "[[Spec]] and [[Issue]]"
        );
        let link = &wiki_links("[[Issue #3|x]]")[0];
        assert_eq!(link.target, "Issue");
        assert_eq!(link.heading.as_deref(), Some("3"));
        assert_eq!(link.destination, "Issue #3");
    }

    #[test]
    fn rename_without_matches_is_identity() {
        let md = "No links here, only [text](url).";
        assert_eq!(rename_links(md, "Old", "New"), md);
    }
}
