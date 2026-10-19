//! A small reader and writer for the YAML frontmatter of Markdown files.
//!
//! Only what notes use is read: top-level keys with a scalar, a list (flow `[a, b]` or block
//! `- a`) or one level of `key: value` pairs. Everything is kept as text, so a frontmatter Knode
//! does not understand is written back exactly as it was read.

/// A Markdown file split into its frontmatter and its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Split<'a> {
    /// The lines between the `---` delimiters, each with its line ending, if the file has a
    /// frontmatter.
    pub frontmatter: Option<&'a str>,
    /// Everything after the closing `---` line, or the whole file.
    pub body: &'a str,
    /// The line ending of the file: `"\r\n"` when its first line ends so, else `"\n"`.
    pub newline: &'static str,
}

/// Splits a file into frontmatter and body. A frontmatter starts with a `---` line at the very
/// start of the file and ends at the next `---` line.
pub(crate) fn split(text: &str) -> Split<'_> {
    let newline = match text.find('\n') {
        Some(i) if i > 0 && text.as_bytes()[i - 1] == b'\r' => "\r\n",
        _ => "\n",
    };
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"));
    if let Some(rest) = rest {
        let mut offset = 0;
        for line in rest.split_inclusive('\n') {
            if line.trim_end_matches(['\n', '\r']) == "---" {
                return Split {
                    frontmatter: Some(&rest[..offset]),
                    body: &rest[offset + line.len()..],
                    newline,
                };
            }
            offset += line.len();
        }
    }
    Split {
        frontmatter: None,
        body: text,
        newline,
    }
}

/// One top-level entry of a frontmatter: a `key:` line and the lines that belong to it
/// (indented lines, list items, comments and blank lines that follow it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry<'a> {
    /// The key, unquoted; `None` for lines before the first key.
    pub key: Option<String>,
    /// The entry exactly as written, with its line endings.
    pub text: &'a str,
}

impl Entry<'_> {
    /// The text after `key:` on the first line, trimmed.
    fn inline(&self) -> &str {
        let first = self.text.lines().next().unwrap_or("");
        first.split_once(':').map(|(_, v)| v.trim()).unwrap_or("")
    }

    /// The lines after the first one, without line endings.
    fn nested(&self) -> impl Iterator<Item = &str> {
        self.text.lines().skip(1).map(|l| l.trim_end_matches('\r'))
    }

    /// The value as one scalar, if it is one.
    pub fn scalar(&self) -> Option<String> {
        let inline = self.inline();
        if inline.is_empty() || inline.starts_with('[') || inline.starts_with('{') {
            return None;
        }
        let value = parse_scalar(inline);
        Some(value)
    }

    /// The value as a list: a flow list, a block list, or a scalar of words separated by commas
    /// or spaces.
    pub fn list(&self) -> Vec<String> {
        let inline = self.inline();
        if inline.starts_with('[') {
            return parse_flow_list(inline);
        }
        if !inline.is_empty() {
            let value = parse_scalar(inline);
            return value
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
        }
        self.nested()
            .filter_map(|line| line.trim_start().strip_prefix('-'))
            .map(|item| parse_scalar(item.trim()))
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// The value as `key: value` pairs, one level deep. Values are left as written, to be read
    /// with [`parse_scalar`] or [`parse_flow_list`].
    pub fn map(&self) -> Vec<(String, String)> {
        self.nested()
            .filter(|line| line.starts_with([' ', '\t']))
            .filter_map(|line| {
                let line = line.trim();
                if line.starts_with('#') {
                    return None;
                }
                let (key, value) = split_key(line)?;
                Some((key, value.trim().to_string()))
            })
            .collect()
    }
}

/// Splits a frontmatter into its top-level entries. Joining the `text` of every entry gives the
/// frontmatter back exactly.
pub(crate) fn entries(frontmatter: &str) -> Vec<Entry<'_>> {
    let mut out: Vec<Entry<'_>> = Vec::new();
    let mut start = 0;
    let mut key: Option<String> = None;
    let mut offset = 0;
    for line in frontmatter.split_inclusive('\n') {
        let starts_entry = line.starts_with(|c: char| !c.is_whitespace() && c != '-' && c != '#')
            && split_key(line.trim_end()).is_some();
        if starts_entry {
            if offset > start {
                out.push(Entry {
                    key: key.take(),
                    text: &frontmatter[start..offset],
                });
            }
            start = offset;
            key = split_key(line.trim_end()).map(|(k, _)| k);
        }
        offset += line.len();
    }
    if offset > start {
        out.push(Entry {
            key,
            text: &frontmatter[start..offset],
        });
    }
    out
}

/// Splits `key: value` (or `key:` at the end of a line) into an unquoted key and the value.
fn split_key(line: &str) -> Option<(String, &str)> {
    let (key, value) = if let Some(quote @ ('"' | '\'')) = line.chars().next() {
        let end = line[1..].find(quote)? + 1;
        let key = &line[1..end];
        let rest = line[end + 1..].strip_prefix(':')?;
        (key.to_string(), rest)
    } else {
        let colon = line
            .match_indices(':')
            .map(|(i, _)| i)
            .find(|&i| line[i + 1..].is_empty() || line[i + 1..].starts_with([' ', '\t']))?;
        (line[..colon].trim().to_string(), &line[colon + 1..])
    };
    if key.is_empty() {
        return None;
    }
    if !value.is_empty() && !value.starts_with([' ', '\t']) {
        return None;
    }
    Some((key, value))
}

/// Reads one YAML scalar: double-quoted, single-quoted or plain (where ` #` starts a comment).
pub(crate) fn parse_scalar(text: &str) -> String {
    let text = text.trim();
    if text.starts_with('"') {
        if let Some(end) = closing_double_quote(text) {
            if let Ok(value) = serde_json::from_str::<String>(&text[..=end]) {
                return value;
            }
            return text[1..end].to_string();
        }
    } else if let Some(rest) = text.strip_prefix('\'') {
        let mut out = String::new();
        let mut chars = rest.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    chars.next();
                    out.push('\'');
                } else {
                    return out;
                }
            } else {
                out.push(c);
            }
        }
        return out;
    }
    let plain = match text.find(" #") {
        Some(i) => &text[..i],
        None => text,
    };
    plain.trim().to_string()
}

/// The index of the `"` that closes a double-quoted scalar starting at index 0.
fn closing_double_quote(text: &str) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in text.char_indices().skip(1) {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => return Some(i),
            _ => escaped = false,
        }
    }
    None
}

/// Reads a flow list, `[a, "b", 'c']`.
pub(crate) fn parse_flow_list(text: &str) -> Vec<String> {
    let text = text.trim();
    let Some(inner) = text.strip_prefix('[') else {
        return Vec::new();
    };
    let mut items = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in inner.chars() {
        match quote {
            Some(q) => {
                current.push(c);
                if q == '"' && c == '\\' && !escaped {
                    escaped = true;
                    continue;
                }
                if c == q && !escaped {
                    quote = None;
                }
                escaped = false;
            }
            None => match c {
                '"' | '\'' if current.trim().is_empty() => {
                    quote = Some(c);
                    current.push(c);
                }
                ',' | ']' => {
                    let item = parse_scalar(&current);
                    if !item.is_empty() {
                        items.push(item);
                    }
                    current.clear();
                    if c == ']' {
                        return items;
                    }
                }
                _ => current.push(c),
            },
        }
    }
    let item = parse_scalar(&current);
    if !item.is_empty() {
        items.push(item);
    }
    items
}

/// Writes a scalar: plain when that reads back as the same text in any YAML reader, else
/// double-quoted.
pub(crate) fn scalar(value: &str) -> String {
    if is_plain(value) {
        value.to_string()
    } else {
        serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""))
    }
}

/// Writes a flow list, `[a, b]`.
pub(crate) fn flow_list<S: AsRef<str>>(items: &[S]) -> String {
    let items: Vec<String> = items.iter().map(|s| scalar(s.as_ref())).collect();
    format!("[{}]", items.join(", "))
}

fn is_plain(value: &str) -> bool {
    const RESERVED: [&str; 9] = ["true", "false", "null", "yes", "no", "on", "off", "~", "y"];
    let Some(first) = value.chars().next() else {
        return false;
    };
    value.trim() == value
        && !value.chars().any(char::is_control)
        && !"-?:,[]{}#&*!|>'\"%@`\\".contains(first)
        && !value.contains(": ")
        && !value.contains(" #")
        && !value.ends_with(':')
        && !value.contains([',', '[', ']', '{', '}', '"'])
        && !RESERVED.contains(&value.to_lowercase().as_str())
        && value.parse::<f64>().is_err()
        && !value.starts_with("0x")
        && !value.starts_with("0o")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_frontmatter_and_body() {
        let s = split("---\nid: 1\n---\n# Body\n");
        assert_eq!(s.frontmatter, Some("id: 1\n"));
        assert_eq!(s.body, "# Body\n");
        assert_eq!(s.newline, "\n");

        let s = split("---\r\na: b\r\n---\r\nText");
        assert_eq!(s.frontmatter, Some("a: b\r\n"));
        assert_eq!(s.body, "Text");
        assert_eq!(s.newline, "\r\n");

        let s = split("---\n---\n");
        assert_eq!(s.frontmatter, Some(""));
        assert_eq!(s.body, "");

        let s = split("No frontmatter\n---\n");
        assert_eq!(s.frontmatter, None);
        let s = split("---\nnot closed\n");
        assert_eq!(s.frontmatter, None);
    }

    #[test]
    fn entries_keep_the_text() {
        let fm = "# comment\naliases: [a, b]\ntags:\n  - one\n  - Two\n\nurl: https://x.y/#z\n\"quoted key\": v\n";
        let e = entries(fm);
        assert_eq!(e.iter().map(|e| e.text).collect::<String>(), fm);
        let keys: Vec<_> = e.iter().map(|e| e.key.clone()).collect();
        assert_eq!(
            keys,
            [
                None,
                Some("aliases".into()),
                Some("tags".into()),
                Some("url".into()),
                Some("quoted key".into())
            ]
        );
        assert_eq!(e[1].list(), ["a", "b"]);
        assert_eq!(e[2].list(), ["one", "Two"]);
        assert_eq!(e[3].scalar().as_deref(), Some("https://x.y/#z"));
    }

    #[test]
    fn reads_lists_and_maps() {
        let e = entries("tags: one, two three\nprops:\n  a.b: \"x: y\"\n  c: [1, 2]\n");
        assert_eq!(e[0].list(), ["one", "two", "three"]);
        let map = e[1].map();
        assert_eq!(map[0], ("a.b".into(), "\"x: y\"".into()));
        assert_eq!(parse_scalar(&map[0].1), "x: y");
        assert_eq!(parse_flow_list(&map[1].1), ["1", "2"]);
    }

    #[test]
    fn scalars_round_trip() {
        for value in [
            "Plan",
            "01JABCDEFGHJKMNPQRSTVWXYZ0",
            "2026-10-20T09:30:00.000Z",
            "Café de ideas",
            "a: b",
            "#tag",
            "- item",
            "true",
            "42",
            "1e3",
            " padded ",
            "quote \" inside",
            "it's",
            "line\nbreak",
            "[x]",
            "",
        ] {
            let written = scalar(value);
            assert_eq!(parse_scalar(&written), value, "{written}");
            let list = flow_list(&[value, "b"]);
            let read = parse_flow_list(&list);
            if value.is_empty() {
                assert_eq!(read, ["b"]);
            } else {
                assert_eq!(read, [value, "b"], "{list}");
            }
        }
        assert_eq!(scalar("Plan"), "Plan");
        assert_eq!(scalar("true"), "\"true\"");
        assert_eq!(parse_scalar("'it''s'"), "it's");
        assert_eq!(parse_scalar("plain # comment"), "plain");
    }
}
