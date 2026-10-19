//! Small text helpers shared by the parsers.

use chrono::{DateTime, Utc};
use serde_json::Value;

/// The longest tool summary kept, in characters.
pub(crate) const SUMMARY_MAX: usize = 120;

/// Collapses `text` to its first non-empty line, trimmed, and cuts it to `max` characters.
/// Marks with an ellipsis anything left out.
pub(crate) fn one_line(text: &str, max: usize) -> String {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let first = lines.next().unwrap_or("");
    let more = lines.next().is_some();
    let collapsed: String = first.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = collapsed.chars().take(max).collect();
    if more || collapsed.chars().count() > max {
        out.push('…');
    }
    out
}

/// Whether text starts with a tag like `<environment_context>`, which agents use for
/// context they inject into the conversation rather than for what a person typed.
pub(crate) fn starts_with_tag(text: &str) -> bool {
    let text = text.trim_start();
    let Some(rest) = text.strip_prefix('<') else {
        return false;
    };
    let name_len = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(rest.len());
    name_len > 0 && rest[name_len..].starts_with('>')
}

/// The text between `<tag>` and `</tag>`, if present.
pub(crate) fn tag_content<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].trim())
}

/// Parses an RFC 3339 timestamp from a JSON value.
pub(crate) fn timestamp(value: Option<&Value>) -> Option<DateTime<Utc>> {
    let text = value?.as_str()?;
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

/// A time given as an RFC 3339 string or as milliseconds since the Unix epoch.
pub(crate) fn any_time(value: Option<&Value>) -> Option<DateTime<Utc>> {
    match value? {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|n| n as i64))
            .and_then(DateTime::from_timestamp_millis),
        other => timestamp(Some(other)),
    }
}

/// A string field of a JSON object.
pub(crate) fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

/// The first of `keys` that holds a non-empty string.
pub(crate) fn first_str<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .filter_map(|key| str_field(value, key))
        .find(|text| !text.trim().is_empty())
}

/// Decodes `%XX` escapes in a URL path.
pub(crate) fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(Ok(byte)) = text
                .get(i + 1..i + 3)
                .map(|hex| u8::from_str_radix(hex, 16))
            {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_keeps_the_first_line_and_marks_cuts() {
        assert_eq!(one_line("  ls   -la ", 20), "ls -la");
        assert_eq!(one_line("\nfirst\nsecond", 20), "first…");
        assert_eq!(one_line("abcdef", 3), "abc…");
        assert_eq!(one_line("", 3), "");
    }

    #[test]
    fn recognises_injected_tags() {
        assert!(starts_with_tag(
            "<environment_context>\n</environment_context>"
        ));
        assert!(starts_with_tag("  <command-name>/clear</command-name>"));
        assert!(!starts_with_tag("< not a tag"));
        assert!(!starts_with_tag("<>"));
        assert!(!starts_with_tag("plain text"));
    }

    #[test]
    fn reads_times_as_text_or_milliseconds() {
        let text = serde_json::json!("2026-01-02T03:04:05Z");
        let millis = serde_json::json!(1_767_323_045_000_i64);
        assert_eq!(any_time(Some(&text)), any_time(Some(&millis)));
        assert!(any_time(Some(&serde_json::json!(null))).is_none());
    }

    #[test]
    fn reads_tag_content() {
        let text = "<command-name>/review</command-name><command-args> 12 </command-args>";
        assert_eq!(tag_content(text, "command-name"), Some("/review"));
        assert_eq!(tag_content(text, "command-args"), Some("12"));
        assert_eq!(tag_content(text, "missing"), None);
    }
}
