//! Text and HTML written as Markdown.
//!
//! Text from a PDF or an export is escaped so that it reads back as the same text: `[[` in a
//! highlight does not become a link, and a line that starts with `#` does not become a heading.
//! HTML is walked as a tree and written as Markdown; scripts, styles, forms and embedded objects
//! are dropped, and only `http`, `https` and `mailto` links are kept.

use dom_query::{Document, NodeRef};

/// Escapes the characters of `text` that Markdown would read as inline syntax.
pub(crate) fn escape_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '`' | '*' | '[' | ']' | '<' | '>' | '|') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Escapes `text` as lines of a paragraph: inline syntax, and the start of a line that Markdown
/// would read as a heading, a quote, a list item, a rule or a code block.
pub(crate) fn escape_text(text: &str) -> String {
    text.lines()
        .map(|line| escape_line_start(&escape_inline(line.trim())))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Escapes a line already escaped inline when it starts like a block.
fn escape_line_start(line: &str) -> String {
    let first = line.chars().next();
    match first {
        Some('#' | '-' | '+' | '=' | '_' | '~') => format!("\\{line}"),
        Some(c) if c.is_ascii_digit() => {
            let digits = line.chars().take_while(char::is_ascii_digit).count();
            match line[digits..].chars().next() {
                Some('.' | ')') => format!("{}\\{}", &line[..digits], &line[digits..]),
                _ => line.to_string(),
            }
        }
        _ => line.to_string(),
    }
}

/// `text` as a quote: each line after `> `.
pub(crate) fn quote(text: &str) -> String {
    text.lines()
        .map(|l| {
            if l.is_empty() {
                ">".into()
            } else {
                format!("> {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Elements whose content is never read.
const DROPPED: &[&str] = &[
    "script", "style", "noscript", "template", "iframe", "object", "embed", "svg", "math",
    "canvas", "form", "button", "input", "select", "textarea", "nav", "head", "title", "meta",
    "link", "audio", "video", "source", "track", "dialog",
];

/// Elements that start a block.
const BLOCKS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "body",
    "dd",
    "details",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "html",
    "li",
    "main",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "tr",
    "ul",
    "caption",
];

/// The Markdown of an HTML document or fragment.
pub(crate) fn from_html(html: &str) -> String {
    let doc = Document::from(html);
    let root = doc
        .select("body")
        .nodes()
        .first()
        .cloned()
        .unwrap_or_else(|| doc.root());
    let mut out = Vec::new();
    blocks(&root, &mut out);
    out.join("\n\n")
}

fn name(node: &NodeRef<'_>) -> String {
    node.node_name()
        .map(|n| n.to_ascii_lowercase())
        .unwrap_or_default()
}

fn is_block(node: &NodeRef<'_>) -> bool {
    node.is_element() && BLOCKS.contains(&name(node).as_str())
}

fn is_dropped(node: &NodeRef<'_>) -> bool {
    node.is_element() && DROPPED.contains(&name(node).as_str())
}

/// Writes the blocks of a container: runs of inline content become paragraphs.
fn blocks(node: &NodeRef<'_>, out: &mut Vec<String>) {
    let mut run = String::new();
    for child in node.children() {
        if is_dropped(&child) || child.is_comment() {
            continue;
        }
        if is_block(&child) {
            flush(&mut run, out);
            block(&child, out);
        } else {
            run.push_str(&inline(&child));
        }
    }
    flush(&mut run, out);
}

/// Ends a paragraph.
fn flush(run: &mut String, out: &mut Vec<String>) {
    let text = finish_inline(run);
    if !text.is_empty() {
        out.push(
            text.lines()
                .map(escape_line_start)
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    run.clear();
}

/// A line break inside inline text, kept through whitespace collapsing.
const BREAK: char = '\u{1}';

/// Inline text with its whitespace collapsed and its line breaks written.
fn finish_inline(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .split(BREAK)
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\\\n")
}

fn block(node: &NodeRef<'_>, out: &mut Vec<String>) {
    match name(node).as_str() {
        h @ ("h1" | "h2" | "h3" | "h4" | "h5" | "h6") => {
            let level: usize = h[1..].parse().unwrap_or(2);
            let text = finish_inline(&inline_children(node)).replace("\\\n", " ");
            if !text.is_empty() {
                out.push(format!("{} {text}", "#".repeat(level)));
            }
        }
        "hr" => out.push("---".into()),
        "pre" => {
            let code = node.text().to_string();
            let code = code.trim_matches('\n');
            if code.trim().is_empty() {
                return;
            }
            let language = node
                .first_element_child()
                .and_then(|c| c.attr("class"))
                .or_else(|| node.attr("class"))
                .and_then(|class| {
                    class
                        .split_whitespace()
                        .find_map(|c| c.strip_prefix("language-").map(str::to_string))
                })
                .unwrap_or_default();
            let mut fence = "```".to_string();
            while code.contains(&fence) {
                fence.push('`');
            }
            out.push(format!("{fence}{language}\n{code}\n{fence}"));
        }
        "blockquote" => {
            let mut inner = Vec::new();
            blocks(node, &mut inner);
            if !inner.is_empty() {
                out.push(quote(&inner.join("\n\n")));
            }
        }
        list @ ("ul" | "ol") => {
            let mut items = Vec::new();
            let mut n = node
                .attr("start")
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(1);
            for child in node.element_children() {
                if name(&child) != "li" {
                    continue;
                }
                let mut inner = Vec::new();
                blocks(&child, &mut inner);
                let text = inner.join("\n\n");
                if text.trim().is_empty() {
                    continue;
                }
                let marker = if list == "ol" {
                    let m = format!("{n}. ");
                    n += 1;
                    m
                } else {
                    "- ".to_string()
                };
                let pad = " ".repeat(marker.len());
                let mut lines = text.lines();
                let mut item = format!("{marker}{}", lines.next().unwrap_or(""));
                for line in lines {
                    item.push('\n');
                    if !line.is_empty() {
                        item.push_str(&pad);
                        item.push_str(line);
                    }
                }
                items.push(item);
            }
            if !items.is_empty() {
                out.push(items.join("\n"));
            }
        }
        "table" => table(node, out),
        _ => blocks(node, out),
    }
}

/// A table as a GFM table: the first row is the header.
fn table(node: &NodeRef<'_>, out: &mut Vec<String>) {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for row in node.descendants().into_iter().filter(|d| name(d) == "tr") {
        let cells: Vec<String> = row
            .element_children()
            .into_iter()
            .filter(|c| matches!(name(c).as_str(), "td" | "th"))
            .map(|c| finish_inline(&inline_children(&c)).replace("\\\n", " "))
            .collect();
        if !cells.is_empty() {
            rows.push(cells);
        }
    }
    let Some(width) = rows.iter().map(Vec::len).max() else {
        return;
    };
    let line = |cells: &[String]| {
        let mut cells = cells.to_vec();
        cells.resize(width, String::new());
        format!("| {} |", cells.join(" | "))
    };
    let mut lines = vec![line(&rows[0]), line(&vec!["---".to_string(); width])];
    lines.extend(rows[1..].iter().map(|r| line(r)));
    out.push(lines.join("\n"));
}

fn inline_children(node: &NodeRef<'_>) -> String {
    node.children()
        .iter()
        .filter(|c| !is_dropped(c) && !c.is_comment())
        .map(inline)
        .collect()
}

/// Whether a URL can be kept as a link: absolute, and of a scheme that only names a place.
fn safe_url(url: &str) -> Option<String> {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:"))
        .then(|| {
            if url.contains([' ', '(', ')', '<', '>']) {
                format!("<{}>", url.replace(['<', '>'], ""))
            } else {
                url.to_string()
            }
        })
}

/// Wraps inline text in a delimiter, keeping its outer spaces outside.
fn wrap(text: &str, delimiter: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return text.to_string();
    }
    let lead = if text.starts_with(char::is_whitespace) {
        " "
    } else {
        ""
    };
    let trail = if text.ends_with(char::is_whitespace) {
        " "
    } else {
        ""
    };
    format!("{lead}{delimiter}{trimmed}{delimiter}{trail}")
}

fn inline(node: &NodeRef<'_>) -> String {
    if node.is_text() {
        return escape_inline(&node.text());
    }
    if !node.is_element() || is_dropped(node) {
        return String::new();
    }
    match name(node).as_str() {
        "br" => BREAK.to_string(),
        "strong" | "b" => wrap(&inline_children(node), "**"),
        "em" | "i" | "cite" => wrap(&inline_children(node), "*"),
        "del" | "s" | "strike" => wrap(&inline_children(node), "~~"),
        "code" | "kbd" | "samp" => {
            let text = node.text().to_string();
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if text.is_empty() {
                return String::new();
            }
            let fence = if text.contains('`') { "``" } else { "`" };
            format!("{fence}{text}{fence}")
        }
        "a" => {
            let text = inline_children(node);
            match node.attr("href").and_then(|h| safe_url(&h)) {
                Some(url) if text.trim().is_empty() => {
                    format!("<{}>", url.trim_matches(['<', '>']))
                }
                Some(url) => {
                    let label = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    let lead = if text.starts_with(char::is_whitespace) {
                        " "
                    } else {
                        ""
                    };
                    let trail = if text.ends_with(char::is_whitespace) {
                        " "
                    } else {
                        ""
                    };
                    format!("{lead}[{label}]({url}){trail}")
                }
                None => text,
            }
        }
        "img" => {
            let alt = node
                .attr("alt")
                .map(|a| escape_inline(a.trim()))
                .unwrap_or_default();
            match node.attr("src").and_then(|s| safe_url(&s)) {
                Some(url) if !url.starts_with("mailto:") => format!("![{alt}]({url})"),
                _ => alt,
            }
        }
        _ => inline_children(node),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaped_text_reads_back_as_the_same_text() {
        let text = "# not a heading\n[[Not a link]] and *not emphasis*\n1. not a list\n- nor this";
        let md = escape_text(text);
        assert!(knode_md::wiki_links(&md).is_empty());
        let plain = knode_md::plain_text(&md);
        assert_eq!(
            plain.split_whitespace().collect::<Vec<_>>(),
            text.split_whitespace().collect::<Vec<_>>()
        );
    }

    #[test]
    fn html_becomes_markdown() {
        let html = r#"<html><head><title>T</title><script>alert(1)</script></head><body>
            <h1>Title</h1>
            <p>Some <strong>bold</strong> and <em>soft</em> text with a
               <a href="https://example.com/a b">link</a> and <a href="javascript:alert(1)">no link</a>.</p>
            <script>document.write("never")</script>
            <ul><li>One</li><li>Two <code>x</code></li></ul>
            <ol start="3"><li>Three</li></ol>
            <blockquote><p>Quoted</p></blockquote>
            <pre><code class="language-rust">fn main() {}</code></pre>
            <table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></table>
            <p>Line<br>break</p>
            <img src="data:image/png;base64,AAAA" alt="inline image">
        </body></html>"#;
        let md = from_html(html);
        assert_eq!(
            md,
            "# Title\n\n\
             Some **bold** and *soft* text with a [link](<https://example.com/a b>) and no link.\n\n\
             - One\n- Two `x`\n\n\
             3. Three\n\n\
             > Quoted\n\n\
             ```rust\nfn main() {}\n```\n\n\
             | A | B |\n| --- | --- |\n| 1 | 2 |\n\n\
             Line\\\nbreak\n\n\
             inline image"
        );
        assert!(!md.contains("alert") && !md.contains("never"));
    }
}
