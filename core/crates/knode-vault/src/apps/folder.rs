//! A folder (or `.zip`) of notes an app exported as Markdown: one note per `.md`, `.markdown` or
//! `.txt` file, the files they link to attached.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use knode_format::Timestamp;

use super::markdown::{self, LinkRef, Names};
use super::source::{self, Source};
use super::write::{Batch, Blob, Draft};
use super::{ImportError, NotesReport, io_error};

/// Whether a file of an export is a note.
pub(crate) fn is_note(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown") || lower.ends_with(".txt")
}

/// The stem of a file name.
pub(crate) fn stem(name: &str) -> &str {
    let name = source::file_name(name);
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    }
}

/// The title of a Markdown text and the body without it: a first line `# Title` is the title,
/// else `fallback`.
pub(crate) fn split_title(text: &str, fallback: &str) -> (String, String) {
    let trimmed = text.trim_start_matches(['\n', '\r']);
    let (first, rest) = trimmed.split_once('\n').unwrap_or((trimmed, ""));
    match first.trim_end().strip_prefix("# ") {
        Some(title) if !title.trim().is_empty() => (markdown::title(title), rest.to_string()),
        _ => (markdown::title(fallback), text.to_string()),
    }
}

/// Rewrites the links of a note in folder `dir` of `source`: files of the export become
/// attachments, other notes `[[Title]]` links (by `titles`, path to title).
pub(crate) fn link_files(
    body: &str,
    dir: &str,
    source: &Source,
    titles: &BTreeMap<String, String>,
    names: &mut Names,
    attachments: &mut Vec<(String, Blob)>,
) -> String {
    let mut taken: BTreeMap<String, String> = BTreeMap::new();
    markdown::rewrite_links(body, |link: &LinkRef| {
        if markdown::is_url(&link.dest) || link.dest.starts_with('#') || link.dest.is_empty() {
            return None;
        }
        let dest = link.dest.split('#').next().unwrap_or(&link.dest);
        let decoded = markdown::decode_path(dest);
        let path = [decoded.as_str(), dest]
            .into_iter()
            .filter_map(|d| source::resolve(dir, d))
            .find(|p| source.has(p) || titles.contains_key(p))?;
        if let Some(title) = titles.get(&path) {
            return Some(markdown::wiki(title, &link.text));
        }
        let name = taken
            .entry(path.clone())
            .or_insert_with(|| {
                let name = names.take(source::file_name(&path));
                attachments.push((name.clone(), Blob::Entry(path.clone())));
                name
            })
            .clone();
        Some(if link.image || markdown::is_image(&name) {
            format!("![[{name}]]")
        } else {
            markdown::wiki(&name, &link.text)
        })
    })
}

/// Reads every note of an exported folder or `.zip`. `tags` finds the tags of a body.
pub(crate) fn read(
    path: &Path,
    report: &mut NotesReport,
    tags: fn(&str) -> BTreeSet<String>,
) -> Result<Batch, ImportError> {
    let mut source = Source::open(path).map_err(io_error(path))?;
    let notes: Vec<String> = source
        .names()
        .iter()
        .filter(|n| is_note(n))
        .cloned()
        .collect();
    let mut texts = Vec::new();
    let mut titles = BTreeMap::new();
    for name in &notes {
        match source.read_string(name) {
            Ok(text) => {
                let (title, body) = split_title(&text, stem(name));
                titles.insert(name.clone(), title.clone());
                texts.push((name.clone(), title, body));
            }
            Err(e) => report.skip(name.clone(), e.to_string()),
        }
    }
    let mut drafts = Vec::new();
    for (name, title, body) in texts {
        let dir = source::parent(&name).to_string();
        let mut names = Names::default();
        let mut attachments = Vec::new();
        let body = link_files(&body, &dir, &source, &titles, &mut names, &mut attachments);
        let (created, updated) = source
            .disk_path(&name)
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|meta| {
                let ts = |t: std::io::Result<std::time::SystemTime>| {
                    t.ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| Timestamp(d.as_millis() as i64))
                };
                (ts(meta.created()), ts(meta.modified()))
            })
            .unwrap_or((None, None));
        drafts.push(Draft {
            source_id: name.clone(),
            tags: tags(&body),
            title,
            body: markdown::tidy(&body),
            folder: (!dir.is_empty()).then_some(dir),
            created,
            updated,
            attachments,
            ..Draft::default()
        });
    }
    Ok(Batch {
        drafts,
        source: Some(source),
    })
}
