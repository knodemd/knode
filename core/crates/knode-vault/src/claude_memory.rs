//! The memory Claude Code keeps for each folder it works in, read into a space:
//! [`import_claude_code_memory`].
//!
//! Claude Code keeps what it learns about a folder in
//! `~/.claude/projects/<encoded folder>/memory/*.md` (or under `CLAUDE_CONFIG_DIR` instead of
//! `~/.claude`), where `<encoded folder>` is the absolute folder with every character that is not
//! an ASCII letter or digit replaced by `-`: `/Users/ada/Work/web.app` is
//! `-Users-ada-Work-web-app`. The encoding loses information, so folders are never decoded: the
//! folders of the projects of the space are encoded the same way and compared.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use knode_format::{NodeId, Provenance, Space, folder_kinds, link_kinds};
use serde::{Deserialize, Serialize};

use crate::import::{self, Batch, ImportReport, Outcome, Skipped};
use crate::{Result, io_error};

/// The folder, in `markdown.path`, under which imported memory files are kept:
/// `claude-code-memory/<encoded folder>/<file>`. Importing again finds them there.
pub const PATH_PREFIX: &str = "claude-code-memory";

/// The memory file that only indexes the others: never imported.
pub const INDEX_FILE: &str = "MEMORY.md";

/// The property with the `type` of a memory (`metadata.type` in its frontmatter), such as
/// `user`, `feedback`, `project` or `reference`.
pub const TYPE_PROPERTY: &str = "claude-code.type";

/// The property with the `description` of a memory.
pub const DESCRIPTION_PROPERTY: &str = "claude-code.description";

/// The onboarding receipt a space made by onboarding keeps in its package.
const RECEIPT: &str = "onboarding.json";

/// What happened to one memory file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryOutcome {
    /// It became a new node.
    Created,
    /// It changed the node imported from it before.
    Updated,
    /// The node already had everything.
    Unchanged,
}

/// One memory file read into the space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryNote {
    /// Its node.
    pub id: NodeId,
    /// The node's title: the `name` of the memory, else its file name.
    pub title: String,
    /// The encoded folder the memory belongs to, as Claude Code names it.
    pub folder: String,
    /// The file, inside the folder's `memory` folder.
    pub file: String,
    /// What happened to it.
    pub outcome: MemoryOutcome,
    /// The project the node is `part-of`, by title, if its folder matched one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// What [`import_claude_code_memory`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryReport {
    /// Every memory file read into the space, in order of folder and file.
    pub notes: Vec<MemoryNote>,
    /// How many notes are `part-of` each project, by project title.
    pub linked: BTreeMap<String, usize>,
    /// The encoded folders that matched no project of the space: their notes were imported
    /// without a project.
    pub unmatched: Vec<String>,
    /// The files that could not be imported, and why.
    pub skipped: Vec<Skipped>,
    /// What was left out on purpose: the `MEMORY.md` indexes, and folders (or files) that are
    /// symbolic links to others, which Claude Code reads through the folder they point at.
    pub left_out: Vec<String>,
}

impl MemoryReport {
    /// How many notes have this outcome.
    pub fn count(&self, outcome: MemoryOutcome) -> usize {
        self.notes.iter().filter(|n| n.outcome == outcome).count()
    }
}

/// The folder name Claude Code gives the working folder `path`: every character that is not an
/// ASCII letter or digit becomes `-`. Like Claude Code, a character outside the Basic
/// Multilingual Plane (an emoji) counts as two.
pub fn encode_folder(path: &str) -> String {
    path.encode_utf16()
        .map(|unit| match char::from_u32(u32::from(unit)) {
            Some(c) if c.is_ascii_alphanumeric() => c,
            _ => '-',
        })
        .collect()
}

/// Where Claude Code keeps its folders on this machine, from the environment `var` reads:
/// `$CLAUDE_CONFIG_DIR/projects`, else `$HOME/.claude/projects`.
pub fn default_location(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let set = |name: &str| var(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    let config = set("CLAUDE_CONFIG_DIR").or_else(|| set("HOME").map(|h| h.join(".claude")))?;
    Some(config.join("projects"))
}

/// The `projects` folder for a path given as the `.claude` folder (or `CLAUDE_CONFIG_DIR`) or as
/// its `projects` folder.
pub fn projects_folder(path: &Path) -> PathBuf {
    let inside = path.join("projects");
    if path.file_name().is_some_and(|n| n != "projects") && inside.is_dir() {
        inside
    } else {
        path.to_path_buf()
    }
}

/// Reads the memory of every folder in `projects` (Claude Code's `projects` folder, or the
/// `.claude` folder around it) into `space`, written by `by` with the file as its source.
///
/// - Each `memory/*.md` file but [`INDEX_FILE`] becomes a note, read as any Markdown file is: its
///   frontmatter is kept, its `name` is the title and its `description` and `metadata.type` are
///   also kept as the properties [`DESCRIPTION_PROPERTY`] and [`TYPE_PROPERTY`].
/// - The note's `markdown.path` is `claude-code-memory/<encoded folder>/<file>`, so importing
///   again updates it instead of making a copy.
/// - A note is `part-of` the project of the space its folder belongs to (see
///   [`ProjectMatcher`]). No project is ever created: a folder that matches none is listed in
///   [`unmatched`](MemoryReport::unmatched) and its notes imported without one.
/// - Folders that are symbolic links to other folders are left out.
pub fn import_claude_code_memory(
    space: &mut Space,
    projects: impl AsRef<Path>,
    by: &Provenance,
) -> Result<MemoryReport> {
    let projects = projects_folder(projects.as_ref());
    let matcher = ProjectMatcher::new(space)?;
    let mut report = MemoryReport::default();
    let mut batch = Batch::new(&projects, &[]);
    let mut inner = ImportReport::default();

    let mut folders: Vec<String> = fs::read_dir(&projects)
        .map_err(io_error(&projects))?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();
    folders.sort();
    for folder in folders {
        let dir = projects.join(&folder);
        let Ok(meta) = fs::symlink_metadata(&dir) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            report.left_out.push(folder);
            continue;
        }
        let memory = dir.join("memory");
        if !meta.is_dir() || !fs::symlink_metadata(&memory).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let mut files: Vec<String> = fs::read_dir(&memory)
            .map_err(io_error(&memory))?
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|name| !name.starts_with('.') && name.ends_with(".md"))
            .collect();
        files.sort();
        let project = matcher.project_for(&folder);
        let mut imported = false;
        for file in files {
            let full = memory.join(&file);
            let shown = format!("{folder}/memory/{file}");
            if file == INDEX_FILE {
                report.left_out.push(shown);
                continue;
            }
            match fs::symlink_metadata(&full) {
                Ok(m) if m.is_file() => {}
                Ok(m) if m.file_type().is_symlink() => {
                    report.left_out.push(shown);
                    continue;
                }
                _ => continue,
            }
            let text = match fs::read_to_string(&full) {
                Ok(text) => text,
                Err(e) => {
                    report.skipped.push(Skipped {
                        path: shown,
                        reason: e.to_string(),
                    });
                    continue;
                }
            };
            let rel = format!("{PATH_PREFIX}/{folder}/{file}");
            let mut note = import::parse(&rel, &text);
            note.file_times = import::file_times(&full);
            let meta = memory_meta(&text);
            if let Some(name) = meta.name {
                note.title = name;
            }
            if let Some(description) = meta.description {
                note.props.insert(DESCRIPTION_PROPERTY.into(), description);
            }
            if let Some(kind) = meta.kind {
                note.props.insert(TYPE_PROPERTY.into(), kind);
            }
            let title = note.title.clone();
            let source = by.clone().with_source(full.display().to_string());
            let skipped_before = inner.skipped.len();
            let written =
                batch.note(space, &rel, note, &source, &mut inner, &mut import::no_hook)?;
            for s in inner.skipped.drain(skipped_before..) {
                report.skipped.push(Skipped {
                    path: shown.clone(),
                    reason: s.reason,
                });
            }
            let Some((id, outcome)) = written else {
                continue;
            };
            imported = true;
            if let Some((project_id, project_title)) = &project {
                space.link(&id, project_id, link_kinds::PART_OF, &source)?;
                *report.linked.entry(project_title.clone()).or_default() += 1;
            }
            report.notes.push(MemoryNote {
                id,
                title,
                folder: folder.clone(),
                file,
                outcome: match outcome {
                    Outcome::Created => MemoryOutcome::Created,
                    Outcome::Updated => MemoryOutcome::Updated,
                    Outcome::Unchanged => MemoryOutcome::Unchanged,
                },
                project: project.as_ref().map(|(_, t)| t.clone()),
            });
        }
        if imported && project.is_none() {
            report.unmatched.push(folder);
        }
    }
    batch.finish(space, by, &mut inner)?;
    // Typed links in a frontmatter that could not be made, by the file they are in.
    report
        .skipped
        .extend(inner.unlinked.into_iter().map(|s| Skipped {
            path: source_path(&s.path),
            reason: s.reason,
        }));
    Ok(report)
}

/// `<folder>/memory/<file>` for the `markdown.path` `claude-code-memory/<folder>/<file>`.
fn source_path(rel: &str) -> String {
    let rest = rel
        .strip_prefix(PATH_PREFIX)
        .and_then(|r| r.strip_prefix('/'))
        .unwrap_or(rel);
    match rest.split_once('/') {
        Some((folder, file)) => format!("{folder}/memory/{file}"),
        None => rest.to_string(),
    }
}

/// What the frontmatter of a memory says about it.
#[derive(Debug, Default, PartialEq, Eq)]
struct MemoryMeta {
    name: Option<String>,
    description: Option<String>,
    kind: Option<String>,
}

fn memory_meta(text: &str) -> MemoryMeta {
    use crate::frontmatter::{entries, parse_scalar, split};
    let mut meta = MemoryMeta::default();
    let Some(frontmatter) = split(text).frontmatter else {
        return meta;
    };
    let nonempty = |v: Option<String>| v.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    for entry in entries(frontmatter) {
        match entry.key.as_deref() {
            Some("name") => meta.name = nonempty(entry.scalar()),
            Some("description") => meta.description = nonempty(entry.scalar()),
            Some("metadata") => {
                meta.kind = nonempty(
                    entry
                        .map()
                        .into_iter()
                        .find(|(k, _)| k == "type")
                        .map(|(_, v)| parse_scalar(&v)),
                )
                .or_else(|| flow_map_value(entry.text, "type"));
            }
            // An older layout keeps the type at the top.
            Some("type") if meta.kind.is_none() => meta.kind = nonempty(entry.scalar()),
            _ => {}
        }
    }
    meta
}

/// The value of `key` in a one-line flow map, `metadata: {type: user}`.
fn flow_map_value(text: &str, key: &str) -> Option<String> {
    let (_, value) = text.split_once(':')?;
    let inner = value.trim().strip_prefix('{')?.split('}').next()?;
    inner.split(',').find_map(|pair| {
        let (k, v) = pair.split_once(':')?;
        let k = k.trim().trim_matches(['"', '\'']);
        (k == key)
            .then(|| crate::frontmatter::parse_scalar(v.trim()))
            .filter(|v| !v.is_empty())
    })
}

/// The project folders of a space, encoded as Claude Code encodes folder names, with the project
/// each belongs to.
///
/// - A folder of kind `project` belongs to the project node titled as the folder's last name.
/// - A space made by onboarding keeps its source and groups in `onboarding.json`: each group's
///   folder in the source belongs to the project node made for it.
/// - The longest folder whose encoded path is the encoded name, or the start of it followed by
///   `-` (a folder inside the project), wins.
/// - A space with no folder of kind `project` falls back, for a name no folder matches, to
///   a project node whose title, encoded, is the last part of the encoded name.
pub struct ProjectMatcher {
    /// Encoded folder, project node and title (`None` when no project node matches the folder).
    folders: Vec<(String, Option<(NodeId, String)>)>,
    /// Project nodes, by encoded title, for spaces without folders.
    titles: Vec<(String, NodeId, String)>,
}

impl ProjectMatcher {
    /// Reads the projects, project folders and onboarding receipt of `space`.
    pub fn new(space: &Space) -> Result<ProjectMatcher> {
        let projects = space.projects()?;
        let node_titled = |name: &str| -> Option<(NodeId, String)> {
            let key = name.trim().to_lowercase();
            projects
                .iter()
                .find(|p| p.node.title.trim().to_lowercase() == key)
                .map(|p| (p.node.id, p.node.title.clone()))
        };
        let mut folders = Vec::new();
        let mut has_rules = false;
        let mut add = |path: &Path, project: Option<(NodeId, String)>| {
            let mut paths = vec![path.to_path_buf()];
            if let Ok(real) = path.canonicalize()
                && real != path
            {
                paths.push(real);
            }
            for path in paths {
                let encoded = encode_folder(&path.to_string_lossy());
                if !folders.iter().any(|(e, _)| *e == encoded) {
                    folders.push((encoded, project.clone()));
                }
            }
        };
        for folder in space.folders()? {
            if folder.kind != folder_kinds::PROJECT {
                continue;
            }
            let Some(path) = local_path(&folder.path) else {
                continue;
            };
            has_rules = true;
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            add(&path, node_titled(&name));
        }
        if let Some(receipt) = read_receipt(space.path()) {
            for (path, name) in receipt.folders() {
                let project = receipt
                    .contexts
                    .get(&name)
                    .and_then(|id| NodeId::parse(id).ok())
                    .and_then(|id| projects.iter().find(|p| p.node.id == id))
                    .map(|p| (p.node.id, p.node.title.clone()))
                    .or_else(|| node_titled(&name));
                add(&path, project);
            }
        }
        let titles = if has_rules {
            Vec::new()
        } else {
            projects
                .iter()
                .map(|p| {
                    (
                        encode_folder(&p.node.title),
                        p.node.id,
                        p.node.title.clone(),
                    )
                })
                .filter(|(e, _, _)| e.chars().any(|c| c != '-'))
                .collect()
        };
        Ok(ProjectMatcher { folders, titles })
    }

    /// The project the encoded folder `encoded` belongs to, as its node and title.
    pub fn project_for(&self, encoded: &str) -> Option<(NodeId, String)> {
        let within = |candidate: &str| {
            encoded == candidate
                || encoded
                    .strip_prefix(candidate)
                    .is_some_and(|rest| rest.starts_with('-'))
        };
        let best = self
            .folders
            .iter()
            .filter(|(candidate, _)| within(candidate))
            .max_by_key(|(candidate, _)| candidate.len());
        if let Some((_, project)) = best {
            return project.clone();
        }
        self.titles
            .iter()
            .filter(|(title, _, _)| {
                encoded
                    .strip_suffix(title.as_str())
                    .is_some_and(|rest| rest.ends_with('-'))
            })
            .max_by_key(|(title, _, _)| title.len())
            .map(|(_, id, title)| (*id, title.clone()))
    }
}

/// A folder recorded as `~/...` or absolute, on this machine.
fn local_path(recorded: &str) -> Option<PathBuf> {
    let recorded = recorded.trim();
    let home = || {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|h| h.is_absolute())
    };
    if recorded == "~" {
        return home();
    }
    if let Some(rest) = recorded.strip_prefix("~/") {
        return home().map(|h| h.join(rest));
    }
    let path = PathBuf::from(recorded);
    path.is_absolute().then_some(path)
}

/// The parts of an onboarding receipt that say where each project came from.
#[derive(Debug, Deserialize)]
struct Receipt {
    plan: ReceiptPlan,
    #[serde(default)]
    contexts: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct ReceiptPlan {
    mode: String,
    source: Option<PathBuf>,
    #[serde(default)]
    groups: Vec<ReceiptGroup>,
}

#[derive(Debug, Deserialize)]
struct ReceiptGroup {
    id: String,
    name: String,
}

impl Receipt {
    /// Each group's folder in the source, with the group's project name. In a single project
    /// the source is the project; otherwise each group id is its folder in the source.
    fn folders(&self) -> Vec<(PathBuf, String)> {
        let Some(source) = &self.plan.source else {
            return Vec::new();
        };
        let mut seen = BTreeSet::new();
        self.plan
            .groups
            .iter()
            .filter_map(|g| {
                let path = if self.plan.mode == "project" {
                    source.clone()
                } else {
                    source.join(&g.id)
                };
                seen.insert(path.clone())
                    .then(|| (path, g.name.trim().to_string()))
            })
            .collect()
    }
}

fn read_receipt(package: &Path) -> Option<Receipt> {
    let bytes = fs::read(package.join(RECEIPT)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_encode_as_claude_code_does() {
        assert_eq!(
            encode_folder("/Users/ada/Work/web.app"),
            "-Users-ada-Work-web-app"
        );
        assert_eq!(
            encode_folder("/Users/ada/Projects/DAV+"),
            "-Users-ada-Projects-DAV-"
        );
        assert_eq!(encode_folder("/a/Café"), "-a-Caf-");
        assert_eq!(encode_folder("/a/🙂"), "-a---");
    }

    #[test]
    fn the_location_follows_claude_config_dir() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                vars.iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        assert_eq!(
            default_location(env(&[("HOME", "/h")])),
            Some(PathBuf::from("/h/.claude/projects"))
        );
        assert_eq!(
            default_location(env(&[("HOME", "/h"), ("CLAUDE_CONFIG_DIR", "/c")])),
            Some(PathBuf::from("/c/projects"))
        );
        assert_eq!(
            default_location(env(&[("HOME", "/h"), ("CLAUDE_CONFIG_DIR", "")])),
            Some(PathBuf::from("/h/.claude/projects"))
        );
        assert_eq!(default_location(env(&[])), None);
    }

    #[test]
    fn memory_frontmatter() {
        let text = "---\nname: Build steps\ndescription: \"How to build\"\nmetadata:\n  type: project\n---\nbody\n";
        assert_eq!(
            memory_meta(text),
            MemoryMeta {
                name: Some("Build steps".into()),
                description: Some("How to build".into()),
                kind: Some("project".into()),
            }
        );
        let flow = "---\nname: x\nmetadata: {type: feedback, origin: y}\n---\n";
        assert_eq!(memory_meta(flow).kind.as_deref(), Some("feedback"));
        assert_eq!(memory_meta("no frontmatter"), MemoryMeta::default());
    }
}
