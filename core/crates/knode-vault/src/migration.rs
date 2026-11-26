//! The report of a Markdown folder import, for a person checking a migration: [`report`].
//!
//! [`import`](crate::import) says what it did to the space. A person moving an Obsidian vault to
//! Knode also needs to know where each file went, what was left out and why, and which links and
//! attachments did not come through. [`report`] reads that from the folder, the space and the
//! [`ImportReport`] after the import, without changing either.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use knode_format::{NodeId, Space, link_kinds};
use serde::{Deserialize, Serialize};

use crate::apps::markdown::{decode_path, is_url};
use crate::attachments::links;
use crate::names::{folder_of, join, normalize};
use crate::property_keys as keys;
use crate::{ImportReport, Result, Skipped, io_error};

/// The version of [`MigrationReport`]: a new field keeps it, a changed meaning raises it.
pub const REPORT_VERSION: u32 = 1;

/// What happened to one file of the folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    /// A note that became a new node.
    Created,
    /// A note that changed its node.
    Updated,
    /// A note its node already matched, or whose node was edited in Knode since the last import
    /// while the file did not change (see the reason).
    Unchanged,
    /// A note that changed since the last import, whose node was edited in Knode too: the node
    /// was left as it is, and the file's changes were not brought in.
    Edited,
    /// A file attached to the note that uses it.
    Attached,
    /// A file left out on purpose, such as a hidden folder or a file no note uses.
    Skipped,
    /// A file that could not be imported, such as a note that is not UTF-8 text.
    Failed,
}

/// One file of the folder, and what became of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// The file, relative to the folder, with `/` between folders.
    pub path: String,
    /// What happened to it.
    pub status: FileStatus,
    /// The node of a note (for a note deleted in Knode, the node that was deleted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeId>,
    /// Why, when it was not simply imported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// How many files ended in each [`FileStatus`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    /// Notes that became new nodes.
    pub created: usize,
    /// Notes that changed their node.
    pub updated: usize,
    /// Notes their node already matched, `kept` included.
    pub unchanged: usize,
    /// Of `unchanged`, the notes whose node was edited in Knode since the last import.
    pub kept: usize,
    /// Notes that changed while their node was edited in Knode too, left for a person to merge.
    pub edited: usize,
    /// Files attached to notes.
    pub attached: usize,
    /// Files left out on purpose.
    pub skipped: usize,
    /// Files that could not be imported.
    pub failed: usize,
    /// Of `skipped`, the notes imported before whose node was deleted in Knode: not imported
    /// again.
    #[serde(default)]
    pub deleted: usize,
}

/// A `[[link]]` in a note that names no note of the space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnresolvedLink {
    /// The note it is in.
    pub path: String,
    /// What it names, as written.
    pub target: String,
}

/// A `[[link]]` in a note that names several notes of the space by title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AmbiguousLink {
    /// The note it is in.
    pub path: String,
    /// What it names, as written.
    pub target: String,
    /// The note it points at: the one nearest by folder, as Obsidian resolves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
    /// Every note with that title, by path (or title, for a node without one).
    pub candidates: Vec<String>,
}

/// A file a note embeds or links to that is not attached to its node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingAttachment {
    /// The note.
    pub path: String,
    /// The file, as the note names it.
    pub target: String,
    /// Files of the folder that could be it: same path, or same name elsewhere.
    pub found: Vec<String>,
    /// Why it is not attached.
    pub reason: String,
}

/// Where each file of a Markdown folder went, what was left out and why, and the links and
/// attachments that did not come through: see [`report`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationReport {
    /// [`REPORT_VERSION`].
    pub version: u32,
    /// The folder imported.
    pub source: PathBuf,
    /// How many files ended in each status.
    pub counts: Counts,
    /// Every file of the folder, hidden ones included, sorted by path.
    pub files: Vec<FileEntry>,
    /// `[[links]]` that name no note.
    pub unresolved_links: Vec<UnresolvedLink>,
    /// `[[links]]` that name several notes.
    pub ambiguous_links: Vec<AmbiguousLink>,
    /// Embedded or linked files that are not attached.
    pub missing_attachments: Vec<MissingAttachment>,
    /// The typed links of the frontmatter `links` that could not be made, and why.
    pub failed_links: Vec<Skipped>,
    /// What this report does not check.
    pub limits: Vec<String>,
}

/// The reason of a note imported before whose node was deleted in Knode.
pub const DELETED_REASON: &str =
    "deleted in Knode, not imported again: import with --include-deleted to bring it back";

const LIMITS: &[&str] = &[
    "Obsidian settings, plugins, themes, workspaces and canvases are not migrated; plugin queries stay as text.",
    "Only files next to the note that names them are attached; shared attachment folders are listed as missing attachments.",
    "This report checks the import, not a round trip: compare an export with `knode export <dir> --compare <vault>`.",
];

/// The report of `import`, the result of importing the folder `dir` into `space`. Reads the
/// folder and the space; changes neither.
pub fn report(space: &Space, dir: &Path, import: &ImportReport) -> Result<MigrationReport> {
    let mut files: Vec<FileEntry> = Vec::new();
    let created: BTreeSet<&String> = import.created.iter().collect();
    let updated: BTreeSet<&String> = import.updated.iter().collect();
    let kept: BTreeSet<&String> = import.kept.iter().collect();
    let edited: BTreeSet<&String> = import.edited.iter().collect();
    for (path, id) in &import.nodes {
        let (status, reason) = if created.contains(path) {
            (FileStatus::Created, None)
        } else if updated.contains(path) {
            (FileStatus::Updated, None)
        } else if edited.contains(path) {
            (
                FileStatus::Edited,
                Some("changed in the folder and in Knode since the last import: Knode's version kept, merge by hand".into()),
            )
        } else if kept.contains(path) {
            (
                FileStatus::Unchanged,
                Some("edited in Knode since the last import; the file did not change".into()),
            )
        } else {
            (FileStatus::Unchanged, None)
        };
        files.push(FileEntry {
            path: path.clone(),
            status,
            node: Some(*id),
            reason,
        });
    }
    for (path, id) in &import.deleted {
        files.push(FileEntry {
            path: path.clone(),
            status: FileStatus::Skipped,
            node: Some(*id),
            reason: Some(DELETED_REASON.into()),
        });
    }
    for path in &import.attached {
        files.push(FileEntry {
            path: path.clone(),
            status: FileStatus::Attached,
            node: None,
            reason: None,
        });
    }
    for path in &import.attachments {
        files.push(FileEntry {
            path: path.clone(),
            status: FileStatus::Skipped,
            node: None,
            reason: Some(left_out(path).into()),
        });
    }
    let listed: BTreeSet<String> = files.iter().map(|f| f.path.clone()).collect();
    for s in &import.skipped {
        if !listed.contains(&s.path) {
            files.push(FileEntry {
                path: s.path.clone(),
                status: FileStatus::Failed,
                node: None,
                reason: Some(s.reason.clone()),
            });
        } else if let Some(entry) = files.iter_mut().find(|f| f.path == s.path) {
            // An attachment that could not be attached.
            entry.status = FileStatus::Failed;
            entry.reason = Some(s.reason.clone());
        }
    }
    excluded(dir, "", &mut files)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let mut counts = Counts {
        kept: import.kept.len(),
        deleted: import.deleted.len(),
        ..Counts::default()
    };
    for file in &files {
        *match file.status {
            FileStatus::Created => &mut counts.created,
            FileStatus::Updated => &mut counts.updated,
            FileStatus::Unchanged => &mut counts.unchanged,
            FileStatus::Edited => &mut counts.edited,
            FileStatus::Attached => &mut counts.attached,
            FileStatus::Skipped => &mut counts.skipped,
            FileStatus::Failed => &mut counts.failed,
        } += 1;
    }

    let others: BTreeSet<&str> = import
        .attached
        .iter()
        .chain(&import.attachments)
        .map(String::as_str)
        .collect();
    let failed: BTreeSet<&str> = files
        .iter()
        .filter(|f| f.status == FileStatus::Failed)
        .map(|f| f.path.as_str())
        .collect();
    let mut unresolved_links = Vec::new();
    let mut ambiguous_links = Vec::new();
    let mut missing_attachments = Vec::new();
    for (path, id) in &import.nodes {
        let Some(node) = space.get_node(id)? else {
            continue;
        };
        let names: BTreeSet<String> = space.attachments(id)?.into_iter().map(|a| a.name).collect();
        let files_named = missing(path, &node.body, &names, &others, &failed);
        let file_targets: BTreeSet<String> = files_named.iter().map(|m| m.target.clone()).collect();
        missing_attachments.extend(files_named);
        for target in space.unresolved_links(id)? {
            if !file_targets.contains(&target) && !looks_like_file(&target) {
                unresolved_links.push(UnresolvedLink {
                    path: path.clone(),
                    target,
                });
            }
        }
        ambiguous_links.extend(ambiguous(space, path, id, &node.body, &names)?);
    }
    // A file left out that a note next to it names by another path: say so, as its missing
    // attachment does.
    for m in &missing_attachments {
        if m.reason != OTHER_PATH {
            continue;
        }
        for entry in files.iter_mut().filter(|f| {
            f.status == FileStatus::Skipped
                && m.found.contains(&f.path)
                && folder_of(&f.path) == folder_of(&m.path)
        }) {
            entry.reason = Some(NAMED_BY_OTHER_PATH.into());
        }
    }

    Ok(MigrationReport {
        version: REPORT_VERSION,
        source: dir.to_path_buf(),
        counts,
        files,
        unresolved_links,
        ambiguous_links,
        missing_attachments,
        failed_links: import.unlinked.clone(),
        limits: LIMITS.iter().map(|s| s.to_string()).collect(),
    })
}

/// Why a file a note names is not attached when a file of that name is next to the note, but the
/// note names another path.
const OTHER_PATH: &str =
    "a file of that name is next to the note, but the note names another path: not attached";

/// The same, for that file.
const NAMED_BY_OTHER_PATH: &str = "not a note: a note next to it names a file of this name by another path, so it is not attached";

/// Why a file that is not a note was left out.
fn left_out(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    if lower.ends_with(".canvas") {
        "Obsidian canvas: not converted"
    } else if lower.ends_with(".md") {
        "only files ending in lowercase .md are read as notes"
    } else {
        "not a note, and no note next to it embeds or links to it"
    }
}

/// Adds the paths under `dir` that import never reads: hidden files and folders, symbolic
/// links, `.knode` packages and names that are not UTF-8.
pub(crate) fn excluded(dir: &Path, prefix: &str, out: &mut Vec<FileEntry>) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map_err(io_error(dir))?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            out.push(FileEntry {
                path: join(prefix, &name.to_string_lossy()),
                status: FileStatus::Skipped,
                node: None,
                reason: Some("the name is not UTF-8".into()),
            });
            continue;
        };
        let rel = join(prefix, name);
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let reason = if name.starts_with('.') {
            Some(if meta.is_dir() {
                "hidden folder, such as Obsidian's settings or trash: not imported"
            } else {
                "hidden file: not imported"
            })
        } else if meta.file_type().is_symlink() {
            Some("symbolic link: not followed")
        } else if meta.is_dir()
            && Path::new(name).extension().and_then(|e| e.to_str()) == Some(knode_format::EXTENSION)
        {
            Some("a Knode space: not imported")
        } else if !meta.is_dir() && !meta.is_file() {
            Some("not a regular file")
        } else {
            None
        };
        match reason {
            Some(reason) => out.push(FileEntry {
                path: rel,
                status: FileStatus::Skipped,
                node: None,
                reason: Some(reason.into()),
            }),
            None if meta.is_dir() => excluded(&entry.path(), &rel, out)?,
            None => {}
        }
    }
    Ok(())
}

/// Whether a link target names a file other than a note: it has an extension other than `.md`.
fn looks_like_file(target: &str) -> bool {
    let name = target.rsplit('/').next().unwrap_or(target);
    match name.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && !ext.is_empty()
                && ext.len() <= 5
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
                && !ext.eq_ignore_ascii_case("md")
        }
        None => false,
    }
}

/// The files the note at `path` embeds or links to that its node does not have as attachments.
fn missing(
    path: &str,
    body: &str,
    names: &BTreeSet<String>,
    others: &BTreeSet<&str>,
    failed: &BTreeSet<&str>,
) -> Vec<MissingAttachment> {
    let mut wanted: BTreeSet<String> = knode_md::wiki_links(body)
        .into_iter()
        .map(|l| l.target)
        .filter(|t| looks_like_file(t))
        .collect();
    for (_, dest) in links(body) {
        if is_url(&dest) || dest.starts_with(['#', '/']) {
            continue;
        }
        let dest = dest.split(['#', '?']).next().unwrap_or_default();
        let dest = decode_path(dest.strip_prefix("./").unwrap_or(dest));
        if looks_like_file(&dest) {
            wanted.insert(dest);
        }
    }
    let mut out = Vec::new();
    for target in wanted {
        let name = target.rsplit('/').next().unwrap_or(&target).to_string();
        if names.contains(&target) || names.contains(&name) {
            continue;
        }
        let exact = normalize(&join(folder_of(path), &target));
        let mut found: Vec<String> = others
            .iter()
            .filter(|p| {
                Some(**p) == exact.as_deref() || p.rsplit('/').next() == Some(name.as_str())
            })
            .map(|p| p.to_string())
            .collect();
        found.sort();
        let reason = if found.is_empty() {
            "no such file in the folder".to_string()
        } else if found
            .iter()
            .any(|f| folder_of(f) == folder_of(path) && failed.contains(f.as_str()))
        {
            "next to the note but could not be attached: see the failed files".to_string()
        } else if found.iter().any(|f| folder_of(f) == folder_of(path)) {
            OTHER_PATH.to_string()
        } else {
            "elsewhere in the folder: only files next to the note are attached".to_string()
        };
        out.push(MissingAttachment {
            path: path.to_string(),
            target,
            found,
            reason,
        });
    }
    out
}

/// The `[[links]]` of the note at `path` that name several nodes by title.
fn ambiguous(
    space: &Space,
    path: &str,
    id: &NodeId,
    body: &str,
    names: &BTreeSet<String>,
) -> Result<Vec<AmbiguousLink>> {
    let linked: BTreeSet<NodeId> = space
        .links_from(id)?
        .into_iter()
        .filter(|l| l.kind == link_kinds::MENTIONS)
        .map(|l| l.to)
        .collect();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for link in knode_md::wiki_links(body) {
        let target = link.target;
        if target.contains('/')
            || looks_like_file(&target)
            || names.contains(&link.destination)
            || NodeId::parse(&target).is_ok()
            || !seen.insert(knode_md::title_key(&target))
        {
            continue;
        }
        let found = space.find_by_title(&target)?;
        if found.len() < 2 {
            continue;
        }
        let mut by_path: BTreeMap<NodeId, String> = BTreeMap::new();
        for summary in &found {
            let at = space
                .property(&summary.id, keys::PATH)?
                .unwrap_or_else(|| summary.title.clone());
            by_path.insert(summary.id, at);
        }
        let resolved = found
            .iter()
            .find(|s| linked.contains(&s.id))
            .and_then(|s| by_path.get(&s.id).cloned());
        let mut candidates: Vec<String> = by_path.into_values().collect();
        candidates.sort();
        out.push(AmbiguousLink {
            path: path.to_string(),
            target,
            resolved,
            candidates,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_told_by_their_extension() {
        assert!(looks_like_file("picture.png"));
        assert!(looks_like_file("assets/report.pdf"));
        assert!(!looks_like_file("Plan"));
        assert!(!looks_like_file("Plan.md"));
        assert!(!looks_like_file("v0.2 release notes"));
        assert!(!looks_like_file(".hidden"));
    }

    #[test]
    fn paths_fold_dots() {
        assert_eq!(normalize("A/../B/./c.png").as_deref(), Some("B/c.png"));
        assert_eq!(normalize("../c.png"), None);
    }
}
