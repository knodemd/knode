//! Cutting a node into the passages that are embedded.
//!
//! The rule is `knode-1` of the `.knode` format (spec section 19.3), implemented in
//! `knode-format` so that every writer cuts the same way: sections start at headings, whole
//! sections are packed into chunks of at most 1,000 characters, a longer section is cut by
//! blocks and then after whitespace, and each piece of a long section after its first reaches
//! back about 150 characters. Ranges are in characters of the node's plain text. A chunk is
//! embedded as the node's title, its heading path and its text, each on its own line
//! ([`embedding_text`]); a text longer than the model reads is cut by the model.

pub use knode_format::{
    CHUNK_MAX_CHARS, CHUNK_OVERLAP_CHARS, CHUNKING, TextChunk as Chunk, embedding_text,
};

/// Cuts the body of a node, in Markdown, into chunks by rule `knode-1`.
///
/// ```
/// let chunks = knode_embed::chunk("# Plan\n\nShip it.");
/// assert_eq!(chunks[0].heading, "Plan");
/// assert_eq!(chunks[0].text, "Plan\nShip it.");
/// ```
pub fn chunk(markdown: &str) -> Vec<Chunk> {
    knode_format::chunk_markdown(markdown)
}

/// A rough token count that needs no tokenizer: about four characters per token for words in
/// alphabetic scripts, and one token per character for Chinese, Japanese and Korean. It tends to
/// overestimate.
pub fn estimate_tokens(text: &str) -> usize {
    text.split_whitespace()
        .map(|word| {
            let mut cjk = 0usize;
            let mut other = 0usize;
            for c in word.chars() {
                if is_cjk(c) {
                    cjk += 1;
                } else {
                    other += 1;
                }
            }
            cjk + other.div_ceil(4)
        })
        .sum()
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF
        | 0x20000..=0x2FA1F)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_follow_knode_1() {
        let md = "Intro line.\n\n# One\n\nPara a.\n\n## Two\n\nPara b.\n\n# Three\n\nPara c.";
        let chunks = chunk(md);
        // Short sections are packed into one chunk, which takes the first section's heading.
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].heading, "");
        assert_eq!(
            chunks[0].text,
            "Intro line.\nOne\nPara a.\nTwo\nPara b.\nThree\nPara c."
        );
        assert_eq!((chunks[0].start, chunks[0].end), (0, 49));
    }

    #[test]
    fn chunks_hold_at_most_a_thousand_characters_plus_overlap() {
        let section = |name: &str| format!("# {name}\n\n{}\n\n", "palabra larga ".repeat(50));
        let md = (0..6)
            .map(|i| section(&format!("S{i}")))
            .collect::<String>();
        let chunks = chunk(&md);
        assert!(chunks.len() >= 3);
        for c in &chunks {
            assert!(c.text.chars().count() <= CHUNK_MAX_CHARS);
            assert!(c.heading.starts_with('S'));
        }
        let long = "日本語のメモ ".repeat(400);
        for c in chunk(&long) {
            assert!(c.text.chars().count() <= CHUNK_MAX_CHARS + CHUNK_OVERLAP_CHARS);
        }
    }

    #[test]
    fn empty_text_is_one_empty_chunk() {
        let chunks = chunk("");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "");
    }

    #[test]
    fn embedded_text_has_the_title_and_heading() {
        let chunks = chunk("# Q4\n\nHire two people.");
        assert_eq!(
            embedding_text("Plan", &chunks[0]),
            "Plan\nQ4\nQ4\nHire two people."
        );
    }

    #[test]
    fn estimates_count_cjk_per_character() {
        assert_eq!(estimate_tokens("hello"), 2);
        assert_eq!(estimate_tokens("a an the"), 3);
        assert_eq!(estimate_tokens("日本語"), 3);
        assert_eq!(estimate_tokens(""), 0);
    }
}
