//! Chunking rule `knode-1` (spec section 19.3): how the plain text of a node is cut into the
//! chunks that are embedded.

use serde::{Deserialize, Serialize};

/// The name of the chunking rule this library implements, recorded in
/// [`EmbeddingModelSpec::chunking`](crate::EmbeddingModelSpec::chunking).
pub const CHUNKING: &str = "knode-1";

/// The most characters in a chunk, not counting its overlap.
pub const CHUNK_MAX_CHARS: usize = 1_000;

/// How many characters before its start a piece of a long section reaches back.
pub const CHUNK_OVERLAP_CHARS: usize = 150;

/// What joins the headings of a heading path.
const HEADING_SEPARATOR: &str = " > ";

/// A chunk of a node's plain text, cut by rule `knode-1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextChunk {
    /// Its number in the node, from 0.
    pub ord: u32,
    /// Where it starts in the plain text, in characters (Unicode scalar values) from 0.
    pub start: u32,
    /// Where it ends in the plain text, in characters, excluded.
    pub end: u32,
    /// The path of headings above it, outermost first, joined with ` > `; `''` before the first
    /// heading.
    pub heading: String,
    /// Its text: the characters `start..end` of the plain text.
    pub text: String,
}

/// Cuts the body of a node, in Markdown, into chunks by rule `knode-1`.
///
/// The chunks are ranges of the body's plain text (section 8), which is what the space stores
/// as `plain`. A node with no text has one empty chunk, so its title is still embedded.
///
/// ```
/// let chunks = knode_format::chunk_markdown("# Plan\n\nShip it.\n\n## Q4\n\nHire.");
/// assert_eq!(chunks.len(), 1);
/// assert_eq!(chunks[0].heading, "Plan");
/// assert_eq!(chunks[0].text, "Plan\nShip it.\nQ4\nHire.");
/// ```
pub fn chunk_markdown(markdown: &str) -> Vec<TextChunk> {
    let blocks = knode_md::blocks(markdown);
    chunk_blocks(
        &blocks
            .iter()
            .map(|block| (block.text.as_str(), block.heading))
            .collect::<Vec<_>>(),
    )
}

/// Cuts a stored node into chunks: its `plain` text, with the headings read from its Markdown
/// `body`. When the body's blocks do not give back `plain` exactly (text written by another
/// implementation), every line of `plain` is taken as a paragraph, so ranges stay right.
pub fn chunk_node_text(body: &str, plain: &str) -> Vec<TextChunk> {
    let blocks = knode_md::blocks(body);
    let same = blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        == plain;
    if same {
        return chunk_blocks(
            &blocks
                .iter()
                .map(|block| (block.text.as_str(), block.heading))
                .collect::<Vec<_>>(),
        );
    }
    let lines: Vec<(&str, Option<u8>)> = plain.split('\n').map(|line| (line, None)).collect();
    chunk_blocks(&lines)
}

/// The text a chunk is embedded as (19.3, rule 8), without the model's `document_prefix`: the
/// node's title, the chunk's heading and the chunk's text, each on its own line, leaving out a
/// heading that is `''`.
pub fn embedding_text(title: &str, chunk: &TextChunk) -> String {
    let mut out = String::with_capacity(title.len() + chunk.heading.len() + chunk.text.len() + 2);
    out.push_str(title);
    if !chunk.heading.is_empty() {
        out.push('\n');
        out.push_str(&chunk.heading);
    }
    out.push('\n');
    out.push_str(&chunk.text);
    out
}

/// A section: a range of blocks and its heading path.
struct Section {
    heading: String,
    /// Indexes into the blocks, `first..=last`.
    first: usize,
    last: usize,
}

/// Cuts plain text, given as its lines with the level of each heading, by rule `knode-1`.
/// The plain text is the lines joined with `\n`.
pub fn chunk_blocks(blocks: &[(&str, Option<u8>)]) -> Vec<TextChunk> {
    let chars: Vec<char> = blocks
        .iter()
        .enumerate()
        .flat_map(|(i, (text, _))| (i > 0).then_some('\n').into_iter().chain(text.chars()))
        .collect();
    // The character range of each block.
    let mut ranges = Vec::with_capacity(blocks.len());
    let mut at = 0usize;
    for (i, (text, _)) in blocks.iter().enumerate() {
        if i > 0 {
            at += 1;
        }
        let len = text.chars().count();
        ranges.push((at, at + len));
        at += len;
    }

    let mut out = Vec::new();
    let emit = |start: usize, end: usize, heading: &str, out: &mut Vec<TextChunk>| {
        out.push(TextChunk {
            ord: out.len() as u32,
            start: start as u32,
            end: end as u32,
            heading: heading.to_string(),
            text: chars[start..end].iter().collect(),
        });
    };
    if chars.is_empty() {
        emit(0, 0, "", &mut out);
        return out;
    }

    // 1. Sections, with their heading paths.
    let mut sections: Vec<Section> = Vec::new();
    let mut stack: Vec<(u8, &str)> = Vec::new();
    for (i, (text, heading)) in blocks.iter().enumerate() {
        if let Some(level) = heading {
            stack.retain(|(l, _)| l < level);
            stack.push((*level, text));
            sections.push(Section {
                heading: stack
                    .iter()
                    .map(|(_, t)| *t)
                    .collect::<Vec<_>>()
                    .join(HEADING_SEPARATOR),
                first: i,
                last: i,
            });
        } else if let Some(section) = sections.last_mut() {
            section.last = i;
        } else {
            sections.push(Section {
                heading: String::new(),
                first: i,
                last: i,
            });
        }
    }

    // 2. Packing whole sections; 3. long sections cut on their own.
    let mut current: Option<(usize, usize, &str)> = None;
    for section in &sections {
        let (start, end) = (ranges[section.first].0, ranges[section.last].1);
        if let Some((s, _, heading)) = current {
            if end - s <= CHUNK_MAX_CHARS {
                current = Some((s, end, heading));
                continue;
            }
            let (s, e, heading) = current.take().expect("a chunk is open");
            emit(s, e, heading, &mut out);
        }
        if end - start <= CHUNK_MAX_CHARS {
            current = Some((start, end, &section.heading));
            continue;
        }
        for (s, e) in cut_long_section(&chars, &ranges[section.first..=section.last]) {
            emit(s, e, &section.heading, &mut out);
        }
    }
    if let Some((s, e, heading)) = current {
        emit(s, e, heading, &mut out);
    }
    out
}

/// Cuts a section longer than [`CHUNK_MAX_CHARS`] into pieces (rules 3 and 4).
fn cut_long_section(chars: &[char], blocks: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut pieces: Vec<(usize, usize)> = Vec::new();
    // The start of the piece being built, without overlap, and where it ends so far.
    let mut piece: Option<(usize, usize)> = None;
    for &(block_start, block_end) in blocks {
        let mut from = block_start;
        loop {
            if let Some((s, e)) = piece {
                if block_end - s <= CHUNK_MAX_CHARS && from == block_start {
                    piece = Some((s, block_end));
                    break;
                }
                pieces.push((s, e));
                piece = None;
            }
            // The piece starts at `from`, the start of the block or the rest of a cut block.
            if block_end - from <= CHUNK_MAX_CHARS {
                piece = Some((from, block_end));
                break;
            }
            let limit = from + CHUNK_MAX_CHARS; // the character after the 1,000th
            // A whitespace the piece starts with is not a place to cut.
            let cut = (from + 1..limit)
                .rev()
                .find(|&i| chars[i].is_whitespace())
                .map_or(limit, |i| i + 1);
            pieces.push((from, cut));
            from = cut;
        }
    }
    if let Some(p) = piece {
        pieces.push(p);
    }
    // 4. Overlap: every piece after the first reaches back about 150 characters.
    for piece in pieces.iter_mut().skip(1) {
        let start = piece.0;
        let p = start.saturating_sub(CHUNK_OVERLAP_CHARS);
        piece.0 = (p..start)
            .find(|&j| chars[j].is_whitespace())
            .map_or(p, |j| j + 1);
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(chunks: &[TextChunk], text: &str) {
        let chars: Vec<char> = text.chars().collect();
        for chunk in chunks {
            let slice: String = chars[chunk.start as usize..chunk.end as usize]
                .iter()
                .collect();
            assert_eq!(slice, chunk.text);
            assert!(!chunk.text.starts_with('\n') && !chunk.text.ends_with('\n'));
        }
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.ord as usize, i);
        }
    }

    fn words(n: usize, word: &str) -> String {
        vec![word; n].join(" ")
    }

    #[test]
    fn empty_text_has_one_empty_chunk() {
        let chunks = chunk_markdown("");
        assert_eq!(chunks.len(), 1);
        assert_eq!((chunks[0].start, chunks[0].end), (0, 0));
        assert_eq!(chunks[0].heading, "");
        assert_eq!(embedding_text("Title", &chunks[0]), "Title\n");
    }

    #[test]
    fn short_sections_are_packed_and_take_the_first_heading() {
        let md = "Intro.\n\n# Plan\n\nShip.\n\n## Q4\n\nHire.";
        let chunks = chunk_markdown(md);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].heading, "");
        assert_eq!(chunks[0].text, "Intro.\nPlan\nShip.\nQ4\nHire.");
    }

    #[test]
    fn heading_paths_follow_levels() {
        let blocks = [
            ("A", Some(1)),
            ("x", None),
            ("B", Some(2)),
            ("C", Some(3)),
            ("D", Some(2)),
            ("E", Some(1)),
        ];
        // Long sections keep each in a chunk of its own.
        let long = words(300, "palabra");
        let mut with_text = Vec::new();
        for block in blocks {
            with_text.push(block);
            with_text.push((long.as_str(), None));
        }
        let chunks = chunk_blocks(&with_text);
        let headings: Vec<_> = chunks.iter().map(|c| c.heading.as_str()).collect();
        let mut unique = headings.clone();
        unique.dedup();
        assert_eq!(unique, ["A", "A > B", "A > B > C", "A > D", "E"]);
    }

    #[test]
    fn sections_start_a_new_chunk_when_full() {
        let a = words(100, "alpha"); // 599 characters
        let b = words(100, "betas"); // 599 characters
        let md = format!("# One\n\n{a}\n\n# Two\n\n{b}");
        let text = knode_md::plain_text(&md);
        let chunks = chunk_markdown(&md);
        plain(&chunks, &text);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading, "One");
        assert_eq!(chunks[1].heading, "Two");
        assert_eq!(chunks[1].text, format!("Two\n{b}"));
    }

    #[test]
    fn exactly_one_thousand_characters_is_one_chunk() {
        let text = "a".repeat(1_000);
        let chunks = chunk_blocks(&[(text.as_str(), None)]);
        assert_eq!(chunks.len(), 1);
        let text = "a".repeat(1_001);
        let chunks = chunk_blocks(&[(text.as_str(), None)]);
        assert_eq!(chunks.len(), 2);
        // No whitespace: cut at the 1,000th character, the overlap reaches back 150.
        assert_eq!((chunks[0].start, chunks[0].end), (0, 1_000));
        assert_eq!((chunks[1].start, chunks[1].end), (850, 1_001));
    }

    #[test]
    fn long_blocks_are_cut_after_whitespace_with_overlap() {
        // "ab " repeated: whitespace every third character.
        let text = "ab ".repeat(700).trim_end().to_string(); // 2,099 characters
        let chunks = chunk_blocks(&[(text.as_str(), None)]);
        plain(&chunks, &text);
        // The 1,000th character is index 999, an `a`; the last space before it is 998.
        assert_eq!((chunks[0].start, chunks[0].end), (0, 999));
        // The next piece starts at 999; p = 849 is an `a`, the first space after it is 851.
        assert_eq!(chunks[1].start, 852);
        for chunk in &chunks {
            assert!(!chunk.text.starts_with(' '));
        }
        let last = chunks.last().unwrap();
        assert_eq!(last.end as usize, text.chars().count());
    }

    #[test]
    fn long_sections_pack_their_blocks() {
        let para = words(50, "uno"); // 199 characters
        let mut md = String::from("# Long\n\n");
        for _ in 0..12 {
            md.push_str(&para);
            md.push_str("\n\n");
        }
        md.push_str("# Short\n\nEnd.");
        let text = knode_md::plain_text(&md);
        let chunks = chunk_markdown(&md);
        plain(&chunks, &text);
        // "Long\n" + 4 paragraphs = 5 + 4 * 199 + 3 = 804; a fifth would be 1,004.
        assert_eq!(chunks[0].text.chars().count(), 804);
        assert!(
            chunks
                .iter()
                .take(chunks.len() - 1)
                .all(|c| c.heading == "Long")
        );
        // The second piece reaches back into the first, starting after a whitespace.
        assert!(chunks[1].start < chunks[0].end);
        assert!(chunks[0].end - chunks[1].start <= 150);
        assert_eq!(chunks.last().unwrap().text, "Short\nEnd.");
        assert_eq!(chunks.last().unwrap().heading, "Short");
        for chunk in &chunks {
            assert!(chunk.text.chars().count() <= CHUNK_MAX_CHARS + CHUNK_OVERLAP_CHARS);
        }
    }

    #[test]
    fn characters_are_unicode_scalar_values() {
        let text = "ñandú café 日本語 ".repeat(120);
        let text = text.trim_end();
        let chunks = chunk_blocks(&[(text, None)]);
        plain(&chunks, text);
        assert!(chunks.len() > 1);
        assert!(chunks[0].end <= 1_000);
    }

    #[test]
    fn embedding_text_has_title_heading_and_text() {
        let chunk = TextChunk {
            ord: 0,
            start: 0,
            end: 4,
            heading: "Plan > Q4".into(),
            text: "Hire".into(),
        };
        assert_eq!(
            embedding_text("Roadmap", &chunk),
            "Roadmap\nPlan > Q4\nHire"
        );
    }

    #[test]
    fn stored_text_from_elsewhere_still_gives_right_ranges() {
        let chunks = chunk_node_text("# Not the same", "Line one\nLine two");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "Line one\nLine two");
        assert_eq!(chunks[0].heading, "");
        let chunks = chunk_node_text("# Plan\n\nText", "Plan\nText");
        assert_eq!(chunks[0].heading, "Plan");
    }
}
