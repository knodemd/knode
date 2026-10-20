//! Splitting Markdown into passages small enough for the embedding model.
//!
//! The rules, in order:
//!
//! 1. YAML front matter (`---` at the very start, up to the next `---` or `...`) is skipped.
//! 2. ATX headings (`#` to `######`, outside fenced code) start a new section. A chunk never
//!    spans two sections, and carries the path of headings above it (`["Plans", "Q3"]`).
//!    Setext headings are treated as text.
//! 3. A section is made of blocks separated by blank lines; a fenced code block is one block, and
//!    the heading line is a block of its own, so the first chunk of a section starts with it.
//! 4. Blocks are packed into chunks of at most `max_tokens`. When a chunk is full, the next one
//!    starts with the last block of the previous one if that block is no longer than
//!    `overlap_tokens`, so a short lead-in paragraph is not cut from what follows it.
//! 5. A block longer than `max_tokens` is cut into windows of whole words, each starting about
//!    `overlap_tokens` before the end of the previous one.
//!
//! Every chunk is a contiguous slice of the input: `text == markdown[start..end]`, with `start`
//! and `end` in bytes.

/// A passage of a Markdown text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// The passage, exactly `markdown[start..end]`.
    pub text: String,
    /// Where it starts in the Markdown, in bytes.
    pub start: usize,
    /// Where it ends in the Markdown, in bytes (exclusive).
    pub end: usize,
    /// The headings above it, outermost first; empty before the first heading.
    pub heading_path: Vec<String>,
}

/// How big chunks are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkOptions {
    /// The most tokens in one chunk.
    pub max_tokens: usize,
    /// How many tokens consecutive chunks may share.
    pub overlap_tokens: usize,
}

impl Default for ChunkOptions {
    /// Sized for a model that reads 512 tokens: up to 444 tokens per chunk, 64 shared.
    fn default() -> Self {
        Self::for_max_tokens(512)
    }
}

impl ChunkOptions {
    /// Options for a model that reads `model_max_tokens`, leaving room for special tokens and
    /// the query or passage prefix, with an eighth of it as overlap.
    pub fn for_max_tokens(model_max_tokens: usize) -> ChunkOptions {
        let overlap_tokens = model_max_tokens / 8;
        ChunkOptions {
            max_tokens: model_max_tokens.saturating_sub(overlap_tokens + 4).max(1),
            overlap_tokens,
        }
    }
}

/// Splits Markdown with the default options, estimating tokens with [`estimate_tokens`].
pub fn chunk(markdown: &str) -> Vec<Chunk> {
    chunk_with(markdown, &ChunkOptions::default())
}

/// Splits Markdown with `options`, estimating tokens with [`estimate_tokens`].
pub fn chunk_with(markdown: &str, options: &ChunkOptions) -> Vec<Chunk> {
    chunk_by(markdown, options, estimate_tokens)
}

/// A rough token count that needs no tokenizer: about four characters per token for words in
/// alphabetic scripts, and one token per character for Chinese, Japanese and Korean. It tends to
/// overestimate, so chunks stay within the model.
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

/// Splits Markdown with `options`, counting tokens with `count`.
pub fn chunk_by(
    markdown: &str,
    options: &ChunkOptions,
    count: impl Fn(&str) -> usize,
) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    for section in sections(markdown) {
        pack(markdown, &section, options, &count, &mut chunks);
    }
    chunks
}

struct Section {
    heading_path: Vec<String>,
    /// Byte ranges of its blocks, in order.
    blocks: Vec<(usize, usize)>,
}

/// Lines of `text` with their byte range, without the line ending.
fn lines(text: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut pos = 0;
    std::iter::from_fn(move || {
        if pos >= text.len() {
            return None;
        }
        let start = pos;
        let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
        pos = end + 1;
        let trimmed = if text[start..end].ends_with('\r') {
            end - 1
        } else {
            end
        };
        Some((start, trimmed))
    })
}

fn front_matter_end(text: &str) -> usize {
    let mut iter = lines(text);
    match iter.next() {
        Some((s, e)) if &text[s..e] == "---" => {}
        _ => return 0,
    }
    for (s, e) in iter {
        if matches!(&text[s..e], "---" | "...") {
            return text[e..].find('\n').map_or(text.len(), |i| e + i + 1);
        }
    }
    0
}

/// `(level, text)` when the line is an ATX heading.
fn heading(line: &str) -> Option<(usize, String)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let level = rest.len() - rest.trim_start_matches('#').len();
    if !(1..=6).contains(&level) {
        return None;
    }
    let after = &rest[level..];
    if !after.is_empty() && !after.starts_with([' ', '\t']) {
        return None;
    }
    let mut title = after.trim();
    // A closing sequence of `#` preceded by a space is not part of the title.
    let without = title.trim_end_matches('#');
    if without.is_empty() || without.ends_with([' ', '\t']) {
        title = without.trim_end();
    }
    Some((level, title.to_string()))
}

/// The opening fence of a code block: its character and length.
fn fence(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let c = trimmed.chars().next()?;
    if c != '`' && c != '~' {
        return None;
    }
    let len = trimmed.len() - trimmed.trim_start_matches(c).len();
    if len < 3 || (c == '`' && trimmed[len..].contains('`')) {
        return None;
    }
    Some((c, len))
}

fn closes_fence(line: &str, open: (char, usize)) -> bool {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return false;
    }
    let len = trimmed.len() - trimmed.trim_start_matches(open.0).len();
    len >= open.1 && trimmed[len..].trim().is_empty()
}

fn sections(markdown: &str) -> Vec<Section> {
    let body_start = front_matter_end(markdown);
    let body = &markdown[body_start..];
    let mut sections = vec![Section {
        heading_path: Vec::new(),
        blocks: Vec::new(),
    }];
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut block: Option<(usize, usize)> = None;
    let mut open_fence: Option<(char, usize)> = None;

    let flush = |block: &mut Option<(usize, usize)>, sections: &mut Vec<Section>| {
        if let Some(range) = block.take() {
            sections.last_mut().expect("one section").blocks.push(range);
        }
    };

    for (s, e) in lines(body) {
        let (s, e) = (s + body_start, e + body_start);
        let line = &markdown[s..e];
        if let Some(open) = open_fence {
            if closes_fence(line, open) {
                open_fence = None;
            }
            block = Some((block.map_or(s, |b| b.0), e));
            continue;
        }
        if let Some(open) = fence(line) {
            open_fence = Some(open);
            block = Some((block.map_or(s, |b| b.0), e));
            continue;
        }
        if let Some((level, title)) = heading(line) {
            flush(&mut block, &mut sections);
            stack.retain(|(l, _)| *l < level);
            stack.push((level, title));
            sections.push(Section {
                heading_path: stack.iter().map(|(_, t)| t.clone()).collect(),
                blocks: vec![(s, e)],
            });
            continue;
        }
        if line.trim().is_empty() {
            flush(&mut block, &mut sections);
        } else {
            block = Some((block.map_or(s, |b| b.0), e));
        }
    }
    flush(&mut block, &mut sections);
    sections.retain(|section| !section.blocks.is_empty());
    sections
}

/// A block of a chunk being built: its byte range and its tokens.
type Block = ((usize, usize), usize);

fn pack(
    markdown: &str,
    section: &Section,
    options: &ChunkOptions,
    count: &impl Fn(&str) -> usize,
    out: &mut Vec<Chunk>,
) {
    let emit = |start: usize, end: usize, out: &mut Vec<Chunk>| {
        out.push(Chunk {
            text: markdown[start..end].to_string(),
            start,
            end,
            heading_path: section.heading_path.clone(),
        });
    };
    let mut current: Vec<Block> = Vec::new();
    for &(start, end) in &section.blocks {
        let tokens = count(&markdown[start..end]);
        if tokens > options.max_tokens {
            if let (Some(first), Some(last)) = (current.first(), current.last()) {
                emit(first.0.0, last.0.1, out);
            }
            current.clear();
            for (s, e) in windows(markdown, start, end, options, count) {
                emit(s, e, out);
            }
            continue;
        }
        let used: usize = current.iter().map(|(_, t)| t).sum();
        if !current.is_empty() && used + tokens > options.max_tokens {
            let (first, last) = (current[0], current[current.len() - 1]);
            emit(first.0.0, last.0.1, out);
            current.clear();
            if can_overlap(first, last, tokens, options) {
                current.push(last);
            }
        }
        current.push(((start, end), tokens));
    }
    if let (Some(first), Some(last)) = (current.first(), current.last()) {
        emit(first.0.0, last.0.1, out);
    }
}

fn can_overlap(first: Block, last: Block, next_tokens: usize, options: &ChunkOptions) -> bool {
    // A chunk of one block is never repeated whole, and the overlap must leave room.
    first != last && last.1 <= options.overlap_tokens && last.1 + next_tokens <= options.max_tokens
}

/// Cuts a long block into windows of whole words.
fn windows(
    markdown: &str,
    start: usize,
    end: usize,
    options: &ChunkOptions,
    count: &impl Fn(&str) -> usize,
) -> Vec<(usize, usize)> {
    let text = &markdown[start..end];
    let mut words: Vec<(usize, usize, usize)> = Vec::new();
    let mut word_start = None;
    for (i, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        match (c.is_whitespace(), word_start) {
            (true, Some(s)) => {
                words.push((start + s, start + i, count(&text[s..i]).max(1)));
                word_start = None;
            }
            (false, None) => word_start = Some(i),
            _ => {}
        }
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let mut j = i;
        let mut used = 0;
        while j < words.len() && (j == i || used + words[j].2 <= options.max_tokens) {
            used += words[j].2;
            j += 1;
        }
        out.push((words[i].0, words[j - 1].1));
        if j == words.len() {
            break;
        }
        // Step back over the last words, up to `overlap_tokens`, always moving forward.
        let mut back = j;
        let mut shared = 0;
        while back > i + 1 && shared + words[back - 1].2 <= options.overlap_tokens {
            shared += words[back - 1].2;
            back -= 1;
        }
        i = back;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(n: usize) -> String {
        (0..n)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn small(max: usize, overlap: usize) -> ChunkOptions {
        ChunkOptions {
            max_tokens: max,
            overlap_tokens: overlap,
        }
    }

    fn word_count(text: &str) -> usize {
        text.split_whitespace().count()
    }

    #[test]
    fn chunks_are_slices_of_the_input() {
        let md = "Intro line.\n\n# One\n\nPara a.\n\n## Two\n\nPara b.\r\n\n# Three\n\nPara c.";
        let chunks = chunk(md);
        for c in &chunks {
            assert_eq!(&md[c.start..c.end], c.text);
        }
        let paths: Vec<_> = chunks.iter().map(|c| c.heading_path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                vec![],
                vec!["One".to_string()],
                vec!["One".to_string(), "Two".to_string()],
                vec!["Three".to_string()],
            ]
        );
        assert_eq!(chunks[1].text, "# One\n\nPara a.");
        assert_eq!(chunks[0].text, "Intro line.");
    }

    #[test]
    fn front_matter_is_skipped() {
        let md = "---\ntitle: x\n---\nBody text.";
        let chunks = chunk(md);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "Body text.");
        assert!(chunk("---\ntitle: x\n---").is_empty());
    }

    #[test]
    fn headings_inside_code_are_text() {
        let md = "# Real\n\n```sh\n# not a heading\n\necho hi\n```\n\nAfter.";
        let chunks = chunk(md);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].heading_path, vec!["Real".to_string()]);
        assert!(chunks[0].text.contains("# not a heading"));
        assert!(chunks[0].text.ends_with("After."));
    }

    #[test]
    fn heading_titles_drop_closing_hashes() {
        assert_eq!(heading("## Plans ##"), Some((2, "Plans".into())));
        assert_eq!(heading("#hashtag"), None);
        assert_eq!(heading("####### seven"), None);
        assert_eq!(heading("#"), Some((1, String::new())));
        assert_eq!(heading("# C#"), Some((1, "C#".into())));
    }

    #[test]
    fn paragraphs_are_packed_up_to_the_limit() {
        let md = format!("{}\n\n{}\n\n{}", words(4), words(4), words(4));
        let chunks = chunk_by(&md, &small(9, 0), word_count);
        assert_eq!(chunks.len(), 2);
        assert_eq!(word_count(&chunks[0].text), 8);
        assert_eq!(word_count(&chunks[1].text), 4);
    }

    #[test]
    fn a_short_last_paragraph_is_repeated_as_overlap() {
        let md = format!("{}\n\n{}\n\n{}", words(6), words(2), words(6));
        let chunks = chunk_by(&md, &small(9, 2), word_count);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[1].start < chunks[0].end, "{chunks:?}");
        assert_eq!(word_count(&chunks[1].text), 8);
    }

    #[test]
    fn long_paragraphs_become_overlapping_windows() {
        let md = words(25);
        let chunks = chunk_by(&md, &small(10, 3), word_count);
        assert!(chunks.len() >= 3);
        for pair in chunks.windows(2) {
            assert!(pair[1].start < pair[0].end, "windows overlap");
            assert!(pair[1].start > pair[0].start, "windows move forward");
        }
        for c in &chunks {
            assert!(word_count(&c.text) <= 10);
        }
        assert!(chunks.last().unwrap().text.ends_with("w24"));
        assert!(chunks[0].text.starts_with("w0 "));
    }

    #[test]
    fn a_giant_word_is_its_own_window() {
        let md = format!("a {} b", "x".repeat(100));
        let chunks = chunk_by(&md, &small(5, 1), estimate_tokens);
        assert!(chunks.iter().any(|c| c.text == "x".repeat(100)));
    }

    #[test]
    fn empty_and_blank_text_give_no_chunks() {
        assert!(chunk("").is_empty());
        assert!(chunk("\n\n   \n").is_empty());
    }

    #[test]
    fn estimates_count_cjk_per_character() {
        assert_eq!(estimate_tokens("hello"), 2);
        assert_eq!(estimate_tokens("a an the"), 3);
        assert_eq!(estimate_tokens("日本語"), 3);
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn default_options_fit_a_512_token_model() {
        let options = ChunkOptions::default();
        assert_eq!(options.overlap_tokens, 64);
        assert_eq!(options.max_tokens, 444);
    }

    #[test]
    fn multibyte_text_is_cut_on_char_boundaries() {
        let md = "ñandú café ".repeat(200);
        for c in chunk_with(&md, &small(20, 4)) {
            assert_eq!(&md[c.start..c.end], c.text);
        }
    }
}
