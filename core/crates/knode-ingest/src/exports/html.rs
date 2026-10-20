//! Turning the small HTML of a Google Takeout activity into Markdown text. Only what the
//! activity pages use is handled: paragraphs, line breaks, headings, lists, emphasis, code and
//! links; every other tag is dropped and its text kept.

/// Markdown text from an HTML fragment.
pub(crate) fn to_markdown(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    let mut in_pre = false;
    let mut lists: Vec<Option<u32>> = Vec::new();
    while let Some(start) = rest.find('<') {
        push_text(&mut out, &rest[..start], in_pre);
        let after = &rest[start..];
        let Some(end) = after.find('>') else {
            push_text(&mut out, after, in_pre);
            rest = "";
            break;
        };
        let tag = &after[1..end];
        rest = &after[end + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        match (name.as_str(), closing) {
            ("br", _) => out.push('\n'),
            ("p" | "div" | "table" | "tr" | "blockquote", _) => block_break(&mut out),
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", false) => {
                block_break(&mut out);
                let level = name[1..].parse::<usize>().unwrap_or(1);
                out.push_str(&"#".repeat(level));
                out.push(' ');
            }
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", true) => block_break(&mut out),
            ("ul", false) => {
                block_break(&mut out);
                lists.push(None);
            }
            ("ol", false) => {
                block_break(&mut out);
                lists.push(Some(0));
            }
            ("ul" | "ol", true) => {
                lists.pop();
                block_break(&mut out);
            }
            ("li", false) => {
                line_break(&mut out);
                out.push_str(&"  ".repeat(lists.len().saturating_sub(1)));
                match lists.last_mut() {
                    Some(Some(number)) => {
                        *number += 1;
                        out.push_str(&format!("{number}. "));
                    }
                    _ => out.push_str("- "),
                }
            }
            ("pre", false) => {
                block_break(&mut out);
                out.push_str("```\n");
                in_pre = true;
            }
            ("pre", true) => {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str("```");
                in_pre = false;
                block_break(&mut out);
            }
            ("code", _) if !in_pre => out.push('`'),
            ("strong" | "b", _) => out.push_str("**"),
            ("em" | "i", _) => out.push('_'),
            ("td" | "th", true) => out.push(' '),
            _ => {}
        }
    }
    push_text(&mut out, rest, in_pre);
    tidy(&out)
}

/// The text of an HTML fragment on one line, for a prompt or a date.
pub(crate) fn to_text(html: &str) -> String {
    to_markdown(html)
        .replace(['*', '`'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn push_text(out: &mut String, text: &str, in_pre: bool) {
    let text = decode_entities(text);
    if in_pre {
        out.push_str(&text);
        return;
    }
    let mut last_space = out.is_empty() || out.ends_with([' ', '\n']);
    for c in text.chars() {
        if c.is_whitespace() && c != '\u{a0}' {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(if c == '\u{a0}' { ' ' } else { c });
            last_space = false;
        }
    }
}

fn line_break(out: &mut String) {
    trim_end_spaces(out);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn block_break(out: &mut String) {
    trim_end_spaces(out);
    if out.is_empty() || out.ends_with("\n\n") {
        return;
    }
    out.push_str(if out.ends_with('\n') { "\n" } else { "\n\n" });
}

fn trim_end_spaces(out: &mut String) {
    while out.ends_with(' ') {
        out.pop();
    }
}

/// Trims every line's end and collapses runs of blank lines.
fn tidy(text: &str) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        out.push_str(line);
    }
    out
}

/// Decodes the HTML entities Takeout writes: the named ones for markup and spaces, and every
/// numeric one.
pub(crate) fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let decoded = after.find(';').filter(|end| *end <= 10).and_then(|end| {
            let entity = &after[1..end];
            let c = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                "emsp" | "ensp" | "thinsp" => Some(' '),
                _ => entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| entity.strip_prefix('#')?.parse::<u32>().ok())
                    .and_then(char::from_u32),
            }?;
            Some((c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_paragraphs_lists_and_emphasis() {
        let html = "<p>Use <strong>three</strong> steps:</p><ol><li>Mix</li><li>Bake &amp; \
                    <em>cool</em></li></ol><ul><li>Note</li></ul><h2>Done</h2><p>Line<br>two</p>";
        assert_eq!(
            to_markdown(html),
            "Use **three** steps:\n\n1. Mix\n2. Bake & _cool_\n\n- Note\n\n## Done\n\nLine\ntwo"
        );
    }

    #[test]
    fn keeps_code_blocks_as_written() {
        let html = "<p>Run:</p><pre><code>fn main() {\n    let x = 1 &lt; 2;\n}</code></pre>";
        assert_eq!(
            to_markdown(html),
            "Run:\n\n```\nfn main() {\n    let x = 1 < 2;\n}\n```"
        );
        assert_eq!(to_markdown("Use <code>ls</code> now"), "Use `ls` now");
    }

    #[test]
    fn decodes_entities() {
        assert_eq!(
            decode_entities("a&nbsp;b &#39;c&#x27; &bogus; &"),
            "a\u{a0}b 'c' &bogus; &"
        );
        assert_eq!(
            to_text("Prompted&nbsp;what is <b>this</b>?"),
            "Prompted what is this?"
        );
    }
}
