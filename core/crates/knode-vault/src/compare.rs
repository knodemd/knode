//! Comparing a Markdown folder with an export of the space it was imported into: [`compare`].
//!
//! After an import, exporting the space and comparing the export with the original folder shows
//! what a person would lose or see changed by moving to Knode. Knode adds an `id` (and sometimes
//! `title`, `kind`, `links` or `properties`) to the frontmatter of each note: that is reported as
//! a metadata difference, apart from changes to the text. What import never reads (hidden
//! folders such as `.obsidian/`) is listed as excluded rather than counted as lost.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::frontmatter::{entries, split};
use crate::import::walk;
use crate::migration::{FileEntry, excluded};
use crate::{Result, io_error};

/// The version of [`Comparison`]: a new field keeps it, a changed meaning raises it.
pub const COMPARISON_VERSION: u32 = 1;

/// How a file of the source compares with the export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Difference {
    /// The same bytes.
    Identical,
    /// A note with the same text after its frontmatter, whose frontmatter differs: usually only
    /// the `id` Knode adds.
    Metadata,
    /// A note whose text after its frontmatter differs, or a file whose bytes differ.
    Changed,
    /// Not at the same path in the export, but there elsewhere with the same text or bytes.
    Moved,
    /// Not in the export.
    Missing,
}

/// One file of the source, compared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Compared {
    /// The file, relative to the source folder, with `/` between folders.
    pub path: String,
    /// How it compares.
    pub difference: Difference,
    /// Where it is in the export, when not at the same path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exported: Option<String>,
    /// Frontmatter keys the export adds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added_keys: Vec<String>,
    /// Frontmatter keys the export drops.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_keys: Vec<String>,
    /// Frontmatter keys whose value differs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_keys: Vec<String>,
    /// For a changed note, the first line of the text after the frontmatter that differs, from 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_changed_line: Option<usize>,
}

/// How many files of the source ended in each [`Difference`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonCounts {
    /// Identical files.
    pub identical: usize,
    /// Notes with only frontmatter differences.
    pub metadata: usize,
    /// Changed files.
    pub changed: usize,
    /// Files found elsewhere in the export.
    pub moved: usize,
    /// Files not in the export.
    pub missing: usize,
    /// Files only in the export.
    pub added: usize,
    /// Files of the source import never reads.
    pub excluded: usize,
}

/// A source folder compared with an export: see [`compare`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comparison {
    /// [`COMPARISON_VERSION`].
    pub version: u32,
    /// The original folder.
    pub source: PathBuf,
    /// The export.
    pub exported: PathBuf,
    /// How many files ended in each difference.
    pub counts: ComparisonCounts,
    /// The notes of the source, sorted by path.
    pub notes: Vec<Compared>,
    /// The other files of the source, such as images, sorted by path.
    pub files: Vec<Compared>,
    /// Files only in the export, not moved from the source: notes made in Knode, attachments
    /// added there.
    pub added: Vec<String>,
    /// Files of the source import never reads, and why: not compared.
    pub excluded: Vec<FileEntry>,
}

impl Comparison {
    /// Whether every note and file of the source is in the export with the same text or bytes:
    /// nothing changed, moved or missing. Frontmatter Knode adds, excluded files and added
    /// files do not count.
    pub fn text_preserved(&self) -> bool {
        self.counts.changed == 0 && self.counts.moved == 0 && self.counts.missing == 0
    }
}

/// Compares the Markdown folder `source` with `exported`, a folder written by
/// [`export`](crate::export). Reads both; changes neither.
pub fn compare(source: &Path, exported: &Path) -> Result<Comparison> {
    let (mut source_notes, mut source_files) = (Vec::new(), Vec::new());
    walk(source, "", &mut source_notes, &mut source_files)?;
    let (mut out_notes, mut out_files) = (Vec::new(), Vec::new());
    walk(exported, "", &mut out_notes, &mut out_files)?;
    let mut unclaimed: BTreeSet<String> = out_notes.iter().chain(&out_files).cloned().collect();
    let read = |dir: &Path, rel: &str| -> Result<Vec<u8>> {
        let path = dir.join(rel);
        fs::read(&path).map_err(io_error(&path))
    };

    let mut notes = Vec::new();
    let mut moved_notes = Vec::new();
    for rel in &source_notes {
        let original = read(source, rel)?;
        if unclaimed.remove(rel) {
            notes.push(compare_note(rel, &original, &read(exported, rel)?));
        } else {
            moved_notes.push((rel.clone(), original));
        }
    }
    let mut files = Vec::new();
    let mut moved_files = Vec::new();
    for rel in &source_files {
        let original = read(source, rel)?;
        if unclaimed.remove(rel) {
            let same = read(exported, rel)? == original;
            files.push(compared(
                rel,
                if same {
                    Difference::Identical
                } else {
                    Difference::Changed
                },
            ));
        } else {
            moved_files.push((rel.clone(), original));
        }
    }

    // Not at the same path: a note with the same text, or a file with the same bytes, elsewhere.
    let mut bodies: BTreeMap<String, String> = BTreeMap::new();
    let mut bytes: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for rel in &unclaimed {
        let content = read(exported, rel)?;
        if rel.ends_with(".md") {
            bodies.insert(rel.clone(), body_of(&content));
        } else {
            bytes.insert(rel.clone(), content);
        }
    }
    for (rel, original) in moved_notes {
        let body = body_of(&original);
        let found = bodies
            .iter()
            .find(|(path, b)| **b == body && unclaimed.contains(*path))
            .map(|(path, _)| path.clone());
        notes.push(moved(&rel, found, &mut unclaimed));
    }
    for (rel, original) in moved_files {
        let found = bytes
            .iter()
            .find(|(path, b)| **b == original && unclaimed.contains(*path))
            .map(|(path, _)| path.clone());
        files.push(moved(&rel, found, &mut unclaimed));
    }
    notes.sort_by(|a, b| a.path.cmp(&b.path));
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let mut skipped = Vec::new();
    excluded(source, "", &mut skipped)?;
    skipped.sort_by(|a, b| a.path.cmp(&b.path));
    let added: Vec<String> = unclaimed.into_iter().collect();

    let mut counts = ComparisonCounts {
        added: added.len(),
        excluded: skipped.len(),
        ..ComparisonCounts::default()
    };
    for c in notes.iter().chain(&files) {
        *match c.difference {
            Difference::Identical => &mut counts.identical,
            Difference::Metadata => &mut counts.metadata,
            Difference::Changed => &mut counts.changed,
            Difference::Moved => &mut counts.moved,
            Difference::Missing => &mut counts.missing,
        } += 1;
    }
    Ok(Comparison {
        version: COMPARISON_VERSION,
        source: source.to_path_buf(),
        exported: exported.to_path_buf(),
        counts,
        notes,
        files,
        added,
        excluded: skipped,
    })
}

fn compared(path: &str, difference: Difference) -> Compared {
    Compared {
        path: path.to_string(),
        difference,
        exported: None,
        added_keys: Vec::new(),
        removed_keys: Vec::new(),
        changed_keys: Vec::new(),
        first_changed_line: None,
    }
}

fn moved(path: &str, found: Option<String>, unclaimed: &mut BTreeSet<String>) -> Compared {
    match found {
        Some(at) => {
            unclaimed.remove(&at);
            Compared {
                exported: Some(at),
                ..compared(path, Difference::Moved)
            }
        }
        None => compared(path, Difference::Missing),
    }
}

/// The text of a note after its frontmatter, with its line endings as written.
fn body_of(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    split(&text).body.to_string()
}

/// The frontmatter entries of a note, by key, as written (without line endings).
fn keys_of(text: &str) -> BTreeMap<String, String> {
    split(text)
        .frontmatter
        .map(entries)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| {
            e.key
                .clone()
                .map(|k| (k, e.text.replace("\r\n", "\n").trim_end().to_string()))
        })
        .collect()
}

fn compare_note(path: &str, original: &[u8], exported: &[u8]) -> Compared {
    if original == exported {
        return compared(path, Difference::Identical);
    }
    let (a, b) = (
        String::from_utf8_lossy(original),
        String::from_utf8_lossy(exported),
    );
    let (body_a, body_b) = (split(&a).body, split(&b).body);
    let (keys_a, keys_b) = (keys_of(&a), keys_of(&b));
    let mut c = compared(path, Difference::Metadata);
    c.added_keys = keys_b
        .keys()
        .filter(|k| !keys_a.contains_key(*k))
        .cloned()
        .collect();
    c.removed_keys = keys_a
        .keys()
        .filter(|k| !keys_b.contains_key(*k))
        .cloned()
        .collect();
    c.changed_keys = keys_a
        .iter()
        .filter(|(k, v)| keys_b.get(*k).is_some_and(|w| w != *v))
        .map(|(k, _)| k.clone())
        .collect();
    if body_a != body_b {
        c.difference = Difference::Changed;
        let (mut la, mut lb) = (body_a.split('\n'), body_b.split('\n'));
        let mut line = 1;
        loop {
            match (la.next(), lb.next()) {
                (Some(x), Some(y)) if x == y => line += 1,
                (None, None) => break,
                _ => {
                    c.first_changed_line = Some(line);
                    break;
                }
            }
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_and_text_are_told_apart() {
        let c = compare_note(
            "a.md",
            b"---\ntags: [x]\n---\nOne\nTwo\n",
            b"---\nid: 01J\ntags: [y]\n---\nOne\nTwo\n",
        );
        assert_eq!(c.difference, Difference::Metadata);
        assert_eq!(c.added_keys, ["id"]);
        assert_eq!(c.changed_keys, ["tags"]);
        let c = compare_note("a.md", b"One\nTwo\n", b"---\nid: 01J\n---\nOne\nThree\n");
        assert_eq!(c.difference, Difference::Changed);
        assert_eq!(c.first_changed_line, Some(2));
    }
}
