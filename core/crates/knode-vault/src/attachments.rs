//! Attachments in Markdown folders: which files a note uses, and `knode:blob/<hash>` URLs turned
//! into file names and back.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Component, Path};

use pulldown_cmark::{Event, Parser, Tag};

/// Whether an attachment named `name` can be written to a folder as a file of that name: not
/// hidden (which import skips, and which could meet `.knode/`), and not a `.md` file (which
/// import would read as a note).
pub(crate) fn is_exportable_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.to_lowercase().ends_with(".md")
        && !name.contains(['/', '\\'])
}

/// Whether `path` is a relative path of an attachment file that stays inside the folder: only
/// plain names, separated by `/`, none hidden, and no `.md` file.
pub(crate) fn is_safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && path
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.'))
        && is_exportable_name(path.rsplit('/').next().unwrap_or(path))
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// `name` with ` n` before its extension: `beds 2.svg`.
pub(crate) fn numbered(name: &str, n: u32) -> String {
    if n < 2 {
        return name.to_string();
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{} {n}{}", &name[..dot], &name[dot..]),
        _ => format!("{name} {n}"),
    }
}

/// How a file name next to the note is written as a Markdown link destination: as it is when it
/// has no space or parenthesis, else between `<` and `>`. `None` when it cannot be written so.
pub(crate) fn destination(name: &str) -> Option<String> {
    if name.contains(['<', '>']) || name.chars().any(char::is_control) {
        return None;
    }
    if name.contains([' ', '(', ')']) {
        Some(format!("<{name}>"))
    } else {
        Some(name.to_string())
    }
}

/// The names a body uses as `![[name]]` or `[[name]]`: the destinations that could be the name of
/// an attachment (no `/`, which names never have).
pub(crate) fn wiki_names(body: &str) -> BTreeSet<String> {
    knode_md::wiki_links(body)
        .into_iter()
        .map(|link| link.destination)
        .filter(|d| !d.is_empty() && !d.contains(['/', '\\', '[', ']', '|', '#']))
        .collect()
}

/// The source ranges of the inline links and images of `body`, with their destinations.
fn links(body: &str) -> Vec<(Range<usize>, String)> {
    Parser::new_ext(body, knode_md::options())
        .into_offset_iter()
        .filter_map(|(event, range)| match event {
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                Some((range, dest_url.into_string()))
            }
            _ => None,
        })
        .collect()
}

/// Rewrites the destinations of the links and images of `body` that `replace` maps, written as
/// `](<written>`, to `](<new>`. Links whose destination is not written inline (reference links)
/// are left as they are.
fn rewrite(body: &str, replace: impl Fn(&str) -> Option<(String, String)>) -> String {
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    for (range, dest) in links(body) {
        let Some((written, new)) = replace(&dest) else {
            continue;
        };
        let text = &body[range.clone()];
        let needle = format!("]({written}");
        if let Some(at) = text.rfind(&needle) {
            let start = range.start + at + 2;
            edits.push((start..start + written.len(), new));
        }
    }
    // Nested links (an image inside a link) can give the same place twice.
    edits.sort_by_key(|(r, _)| r.start);
    edits.dedup_by_key(|(r, _)| r.start);
    let mut out = body.to_string();
    for (range, new) in edits.into_iter().rev() {
        out.replace_range(range, &new);
    }
    out
}

/// Turns each `knode:blob/<hash>` destination whose hash `files` maps into the file name it maps
/// to (written as [`destination`] gives it).
pub(crate) fn blob_urls_to_files(body: &str, files: &BTreeMap<String, String>) -> String {
    if files.is_empty() || !body.contains("knode:blob/") {
        return body.to_string();
    }
    rewrite(body, |dest| {
        let hash = knode_format::blob_url_hash(dest)?;
        let file = destination(files.get(hash)?)?;
        Some((dest.to_string(), file))
    })
}

/// The other way: each destination that is a file name `files` maps to a hash becomes
/// `knode:blob/<hash>`.
pub(crate) fn files_to_blob_urls(body: &str, files: &BTreeMap<String, String>) -> String {
    if files.is_empty() {
        return body.to_string();
    }
    rewrite(body, |dest| {
        let hash = files.get(dest)?;
        Some((destination(dest)?, format!("knode:blob/{hash}")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    #[test]
    fn names_and_paths() {
        assert!(is_exportable_name("beds.svg"));
        assert!(!is_exportable_name(".knode"));
        assert!(!is_exportable_name("notes.MD"));
        assert!(is_safe_path("Projects/beds.svg"));
        assert!(!is_safe_path("../beds.svg"));
        assert!(!is_safe_path(".hidden/beds.svg"));
        assert_eq!(numbered("beds.svg", 2), "beds 2.svg");
        assert_eq!(numbered("README", 3), "README 3");
        assert_eq!(numbered("beds.svg", 1), "beds.svg");
    }

    #[test]
    fn blob_urls_become_file_names_and_back() {
        let body = format!(
            "![Beds](knode:blob/{HASH}) and [plan](knode:blob/{HASH} \"t\") `knode:blob/{HASH}`\n"
        );
        let files = BTreeMap::from([(HASH.to_string(), "beds plan.svg".to_string())]);
        let out = blob_urls_to_files(&body, &files);
        assert_eq!(
            out,
            format!(
                "![Beds](<beds plan.svg>) and [plan](<beds plan.svg> \"t\") `knode:blob/{HASH}`\n"
            )
        );
        let back = BTreeMap::from([("beds plan.svg".to_string(), HASH.to_string())]);
        assert_eq!(files_to_blob_urls(&out, &back), body);
    }

    #[test]
    fn wiki_names_are_whole_destinations() {
        let names = wiki_names("![[beds.svg]] [[Plan#v2]] [[a/b.png]] ![[photo.png|200]]");
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            ["beds.svg", "photo.png"]
        );
    }
}
