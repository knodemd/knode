//! Reading a folder of `.md` files into a space: [`import`].

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::Path;

use knode_format::{
    ActorKind, ImportedNode, LinkKindSpec, MAX_ATTACHMENT_SIZE, NewNode, NodeId, NodeUpdate,
    Properties, Provenance, Space, Timestamp, link_kinds,
};

use serde::{Deserialize, Serialize};

use crate::attachments::{files_to_blob_urls, wiki_names};
use crate::frontmatter::{Entry, entries, parse_flow_list, parse_scalar, split};
use crate::mirror::{read_attachment_map, read_link_kinds};
use crate::names::{folder_of, join, stem_of, title_from_stem};
use crate::property_keys as keys;
use crate::{Error, LINK_KINDS_FILE, Result, STATE_DIR, STYLE_VAULT, io_error, tags, time};

/// A file [`import`] did not bring in, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skipped {
    /// The file, relative to the folder, with `/` between folders.
    pub path: String,
    /// Why it was skipped.
    pub reason: String,
}

/// What [`import`] did. Paths are relative to the folder, with `/` between folders.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReport {
    /// The files that became new nodes.
    pub created: Vec<String>,
    /// The files that changed a node that already existed.
    pub updated: Vec<String>,
    /// How many files matched their node exactly.
    pub unchanged: usize,
    /// The `.md` files that could not be imported.
    pub skipped: Vec<Skipped>,
    /// Files that are not notes, such as images and PDFs, that no note uses: left out.
    pub attachments: Vec<String>,
    /// The files attached to the notes that use them (or already attached).
    #[serde(default)]
    pub attached: Vec<String>,
    /// How many typed links from the `links` of the files were made or already existed.
    pub links: usize,
    /// The kinds of link the space did not define and the import defined.
    #[serde(default)]
    pub link_kinds: Vec<String>,
    /// The typed links of the files that could not be made, by the file they are in, and why:
    /// a target the space does not have, or a kind that cannot be defined.
    #[serde(default)]
    pub unlinked: Vec<Skipped>,
}

/// Reads every `.md` file under `dir` into `space`, written by `by`.
///
/// - Hidden files and folders (`.obsidian/`, `.trash/`, `.git/`, `.knode/`) and `.knode`
///   packages are skipped. Files that are not `.md` are listed in
///   [`attachments`](ImportReport::attachments) and not imported.
/// - The title is the `title` of the frontmatter, else the file name. The body is the file after
///   its frontmatter, as written: `[[Title|alias]]`, `[[Title#heading]]` and `![[embeds]]` stay
///   as text and become links by title.
/// - A file with an `id` in its frontmatter updates the node with that id (or a node imported
///   from a file with that id by an earlier version of Knode) instead of making a new one. When
///   the space has no such node, the new node keeps the id, and for a file written by Knode its
///   `created`, `updated`, `created_by` and `updated_by` too, so a copy of a space has the same
///   ids, dates and authors. A file without an id updates the node imported before from the
///   same path. Importing a folder again never duplicates.
/// - A file written by Knode gives the node its kind, tags, typed links and properties. Any other
///   file (an Obsidian note) gives it the tags of its frontmatter and its inline `#tags`; its
///   frontmatter is kept as it is, to be written back on export.
/// - The path of the file, folders included, is kept in the property
///   [`markdown.path`](crate::property_keys::PATH).
/// - A file a note embeds or links to by name (`![[beds.svg]]`, `[[beds.svg]]`) that is next to
///   the note is attached to its node under that name, and so are the attachments
///   `.knode/files.json` lists for the note (a folder written by [`export`](crate::export)),
///   whose file names in Markdown links become `knode:blob/<hash>` again. An attachment with that
///   name and other content is replaced. Attached files are listed in
///   [`attached`](ImportReport::attached), the other files in
///   [`attachments`](ImportReport::attachments).
/// - A kind of link the space does not define is defined: as `.knode/link-kinds.json` describes
///   it (a folder written by [`export`](crate::export)), else directed, with its name as its
///   label. Kinds defined so are listed in [`link_kinds`](ImportReport::link_kinds); links that
///   cannot be made, in [`unlinked`](ImportReport::unlinked).
///
/// Typed links and attachments are added, never removed. A file that cannot be read or breaks a rule of the
/// format (such as a bad kind) is skipped and listed in [`skipped`](ImportReport::skipped); the
/// rest is imported.
pub fn import(space: &mut Space, dir: impl AsRef<Path>, by: &Provenance) -> Result<ImportReport> {
    let dir = dir.as_ref();
    let mut notes = Vec::new();
    let mut report = ImportReport::default();
    walk(dir, "", &mut notes, &mut report.attachments)?;
    let mut batch = Batch::new(dir, &report.attachments);
    for rel in notes {
        let full = dir.join(&rel);
        let text = match fs::read_to_string(&full) {
            Ok(text) => text,
            Err(e) => {
                report.skipped.push(Skipped {
                    path: rel,
                    reason: e.to_string(),
                });
                continue;
            }
        };
        batch.note(space, &rel, parse(&rel, &text), by, &mut report, &mut no_hook)?;
    }
    batch.finish(space, by, &mut report)?;
    Ok(report)
}

/// Called with a note about to update the node it matches, before the node is written: it may
/// change the note, or write the node itself first. See [`Batch::note`].
pub(crate) type Hook<'a> = dyn FnMut(&mut Space, NodeId, &str, &mut Note) -> Result<()> + 'a;

fn no_hook(_: &mut Space, _: NodeId, _: &str, _: &mut Note) -> Result<()> {
    Ok(())
}

/// Notes read into a space one at a time, with what is left to do once all of them are in:
/// attachments to report and typed links to make.
pub(crate) struct Batch {
    dir: std::path::PathBuf,
    /// The files under the folder that are not notes.
    files: HashSet<String>,
    /// The attachments `.knode/files.json` lists.
    exported: crate::mirror::AttachmentMap,
    attached: BTreeSet<String>,
    pending_links: Vec<(String, NodeId, String, Vec<String>)>,
}

impl Batch {
    /// A batch for the folder `dir`, whose files that are not notes are `files`.
    pub(crate) fn new(dir: &Path, files: &[String]) -> Batch {
        Batch {
            dir: dir.to_path_buf(),
            files: files.iter().cloned().collect(),
            exported: read_attachment_map(dir),
            attached: BTreeSet::new(),
            pending_links: Vec::new(),
        }
    }

    /// Writes one note, read from the file `rel`, to the space: updates the node it matches or
    /// creates one, and attaches the files it uses. `hook` is called before a node is updated.
    /// Returns the node and what happened to it, or `None` when the note was skipped (and
    /// listed in the report).
    pub(crate) fn note(
        &mut self,
        space: &mut Space,
        rel: &str,
        mut note: Note,
        by: &Provenance,
        report: &mut ImportReport,
        hook: &mut Hook<'_>,
    ) -> Result<Option<(NodeId, Outcome)>> {
        let dir = self.dir.clone();
        let files = &self.files;
        // The attachments export wrote for this note, by name, with their files and hashes.
        let written: Vec<(String, String, String)> = note
            .id
            .and_then(|id| self.exported.get(&id.to_string()))
            .into_iter()
            .flatten()
            .filter(|(_, f)| files.contains(&f.path))
            .map(|(name, f)| (name.clone(), f.path.clone(), f.hash.clone()))
            .collect();
        let as_written = note.body.clone();
        let blob_urls = |hashes: &dyn Fn(&str, &str) -> String| -> String {
            let map: BTreeMap<String, String> = written
                .iter()
                .map(|(name, path, hash)| (file_name(path).to_string(), hashes(name, hash)))
                .collect();
            files_to_blob_urls(&as_written, &map)
        };
        note.body = blob_urls(&|_, hash| hash.to_string());
        let (id, outcome) = match write(space, rel, &mut note, by, hook) {
            Ok(written) => written,
            Err(e) => {
                report.skipped.push(Skipped {
                    path: rel.to_string(),
                    reason: e.to_string(),
                });
                return Ok(None);
            }
        };
        // Next to the note, by name, and the files export wrote for it.
        let mut wanted: BTreeMap<String, String> = written
            .iter()
            .map(|(name, path, _)| (name.clone(), path.clone()))
            .collect();
        for name in wiki_names(&note.body) {
            let path = join(folder_of(rel), &name);
            if files.contains(&path) {
                wanted.entry(name).or_insert(path);
            }
        }
        let mut hashes: BTreeMap<String, String> = BTreeMap::new();
        for (name, path) in wanted {
            match attach(space, &id, &name, &dir.join(&path), by) {
                Ok(hash) => {
                    hashes.insert(name, hash);
                    self.attached.insert(path);
                }
                Err(e) => report.skipped.push(Skipped {
                    path,
                    reason: e.to_string(),
                }),
            }
        }
        // A file changed since the export: its links name the blob now attached.
        let fixed = blob_urls(&|name, hash| hashes.get(name).cloned().unwrap_or(hash.into()));
        let mut outcome = outcome;
        if fixed != note.body {
            space.update_node(
                &id,
                NodeUpdate {
                    body: Some(fixed),
                    ..Default::default()
                },
                by,
            )?;
            if matches!(outcome, Outcome::Unchanged) {
                outcome = Outcome::Updated;
            }
        }
        match outcome {
            Outcome::Created => report.created.push(rel.to_string()),
            Outcome::Updated => report.updated.push(rel.to_string()),
            Outcome::Unchanged => report.unchanged += 1,
        }
        for (kind, targets) in std::mem::take(&mut note.links) {
            self.pending_links.push((rel.to_string(), id, kind, targets));
        }
        Ok(Some((id, outcome)))
    }

    /// Lists the files attached and left out, defines the kinds of link the notes use and makes
    /// their typed links.
    pub(crate) fn finish(
        self,
        space: &mut Space,
        by: &Provenance,
        report: &mut ImportReport,
    ) -> Result<()> {
        let attached = self.attached;
        report.attachments.retain(|path| !attached.contains(path));
        report.attached.extend(attached);

        // The kinds the exported space defined, then any other kind the files use.
        for (kind, spec) in read_link_kinds(&self.dir) {
            if space.link_kind(&kind)?.is_some() {
                continue;
            }
            match space.set_link_kind(&spec, by) {
                Ok(_) => report.link_kinds.push(kind),
                Err(e) => report.unlinked.push(Skipped {
                    path: format!("{STATE_DIR}/{LINK_KINDS_FILE}"),
                    reason: format!("link kind {kind}: {e}"),
                }),
            }
        }
        for (rel, from, kind, targets) in self.pending_links {
            if kind == link_kinds::MENTIONS {
                continue;
            }
            if space.link_kind(&kind)?.is_none() {
                let spec = LinkKindSpec::directed(kind.clone(), kind.clone(), "");
                if let Err(e) = space.set_link_kind(&spec, by) {
                    for target in targets {
                        report.unlinked.push(Skipped {
                            path: rel.clone(),
                            reason: format!("{kind} link to {target}: {e}"),
                        });
                    }
                    continue;
                }
                report.link_kinds.push(kind.clone());
            }
            for target in targets {
                let Some(to) = find(space, &target)? else {
                    report.unlinked.push(Skipped {
                        path: rel.clone(),
                        reason: format!("{kind} link to {target}: no such node"),
                    });
                    continue;
                };
                match space.link(&from, &to, &kind, by) {
                    Ok(_) => report.links += 1,
                    Err(e) => report.unlinked.push(Skipped {
                        path: rel.clone(),
                        reason: format!("{kind} link to {target}: {e}"),
                    }),
                }
            }
        }
        Ok(())
    }
}

/// The last part of a relative path.
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Attaches the file at `full` to the node under `name`, replacing an attachment with that name
/// and other content, and returns the hash of its blob.
fn attach(
    space: &mut Space,
    id: &NodeId,
    name: &str,
    full: &Path,
    by: &Provenance,
) -> Result<String> {
    let size = fs::metadata(full).map_err(io_error(full))?.len();
    if size > MAX_ATTACHMENT_SIZE {
        return Err(Error::Space(knode_format::Error::Invalid(format!(
            "{name} is larger than {} MB, the most a space attaches",
            MAX_ATTACHMENT_SIZE / (1024 * 1024)
        ))));
    }
    let bytes = fs::read(full).map_err(io_error(full))?;
    if let Some(current) = space.attachment(id, name)? {
        if space.read_blob(&current.hash).ok().flatten().as_deref() == Some(bytes.as_slice()) {
            return Ok(current.hash);
        }
        space.detach(id, name, by)?;
    }
    Ok(space.attach(id, name, bytes.as_slice(), by)?.hash)
}

/// Lists the notes and other files under `dir`, sorted by path.
pub(crate) fn walk(
    dir: &Path,
    prefix: &str,
    notes: &mut Vec<String>,
    attachments: &mut Vec<String>,
) -> Result<()> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map_err(io_error(dir))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    for name in names {
        let path = dir.join(&name);
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if Path::new(&name).extension().and_then(|e| e.to_str())
                == Some(knode_format::EXTENSION)
            {
                continue;
            }
            walk(&path, &rel, notes, attachments)?;
        } else if meta.is_file() {
            if name.ends_with(".md") {
                notes.push(rel);
            } else {
                attachments.push(rel);
            }
        }
    }
    Ok(())
}

/// A note read from a file.
#[derive(Debug, Default)]
pub(crate) struct Note {
    pub(crate) id: Option<NodeId>,
    pub(crate) title: String,
    pub(crate) kind: Option<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) created: Option<String>,
    pub(crate) updated: Option<String>,
    pub(crate) created_by: Option<Provenance>,
    pub(crate) updated_by: Option<Provenance>,
    pub(crate) links: Vec<(String, Vec<String>)>,
    pub(crate) props: Properties,
    /// The frontmatter entries Knode does not read, as written.
    pub(crate) extra: String,
    /// Whether the frontmatter was not written by Knode.
    pub(crate) vault: bool,
    pub(crate) body: String,
}

/// Whether a frontmatter was written by Knode: it starts with an `id` and has a `kind`,
/// `created` and `updated` as Knode writes them.
fn is_knode(entries: &[Entry<'_>]) -> bool {
    let get = |key: &str| {
        entries
            .iter()
            .find(|e| e.key.as_deref() == Some(key))
            .and_then(Entry::scalar)
    };
    let starts_with_id = entries.first().is_some_and(|e| {
        e.key.as_deref() == Some("id") && e.scalar().is_some_and(|v| NodeId::parse(&v).is_ok())
    });
    starts_with_id
        && get("kind").is_some()
        && get("created").is_some_and(|v| time::parse(&v).is_some())
        && get("updated").is_some_and(|v| time::parse(&v).is_some())
}

pub(crate) fn parse(rel: &str, text: &str) -> Note {
    let parts = split(text);
    let entries = parts.frontmatter.map(entries).unwrap_or_default();
    let mut note = Note {
        title: title_from_stem(stem_of(rel)),
        body: parts.body.to_string(),
        ..Note::default()
    };
    let read_links = |e: &Entry<'_>| -> Vec<(String, Vec<String>)> {
        e.map()
            .into_iter()
            .map(|(kind, value)| (kind, parse_flow_list(&value)))
            .collect()
    };
    let read_props = |e: &Entry<'_>| -> Properties {
        e.map()
            .into_iter()
            .filter(|(key, _)| !key.starts_with(keys::PREFIX))
            .map(|(key, value)| (key, parse_scalar(&value)))
            .collect()
    };
    // The lines after the first of an entry whose value is on its first line.
    let rest_of = |e: &Entry<'_>| -> String {
        e.text
            .split_once('\n')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_default()
    };

    if is_knode(&entries) {
        for entry in &entries {
            match entry.key.as_deref() {
                Some("id") => note.id = entry.scalar().and_then(|v| NodeId::parse(&v).ok()),
                Some("title") => note.title = entry.scalar().unwrap_or(note.title.clone()),
                Some("kind") => note.kind = entry.scalar(),
                Some("tags") => note.tags = entry.list(),
                Some("created") => note.created = entry.scalar(),
                Some("updated") => note.updated = entry.scalar(),
                Some("created_by") => note.created_by = read_provenance(entry),
                Some("updated_by") => note.updated_by = read_provenance(entry),
                Some("links") => note.links = read_links(entry),
                Some("properties") => note.props = read_props(entry),
                _ => note.extra.push_str(entry.text),
            }
        }
        return note;
    }

    // A note from another app: only what Knode adds right after an `id` first line is Knode's.
    note.vault = true;
    let mut managed = true;
    let mut seen = BTreeSet::new();
    for (i, entry) in entries.iter().enumerate() {
        let key = entry.key.as_deref().unwrap_or("");
        let id = (i == 0 && key == "id")
            .then(|| entry.scalar().and_then(|v| NodeId::parse(&v).ok()))
            .flatten();
        let knode_key = match key {
            "kind" => entry.scalar().is_some_and(|v| is_kind(&v)),
            "title" | "links" | "properties" => true,
            _ => false,
        };
        managed = managed && (id.is_some() || (i > 0 && knode_key)) && seen.insert(key.to_string());
        if !managed {
            note.extra.push_str(entry.text);
            continue;
        }
        match key {
            "id" => {
                note.id = id;
                note.extra.push_str(&rest_of(entry));
            }
            "title" | "kind" => {
                let value = entry.scalar();
                if key == "title" {
                    note.title = value.unwrap_or(note.title.clone());
                } else {
                    note.kind = value;
                }
                note.extra.push_str(&rest_of(entry));
            }
            "links" => note.links = read_links(entry),
            _ => note.props = read_props(entry),
        }
    }
    let mut tags: BTreeSet<String> = crate::frontmatter::entries(&note.extra)
        .iter()
        .filter(|e| matches!(e.key.as_deref(), Some("tags" | "tag")))
        .flat_map(Entry::list)
        .filter_map(|t| tags::clean(&t))
        .collect();
    tags.extend(tags::inline(&note.body));
    note.tags = tags.into_iter().collect();
    note
}

/// A provenance written as `kind`, `actor`, `session` and `source` pairs, if it is one.
fn read_provenance(entry: &Entry<'_>) -> Option<Provenance> {
    let mut kind = None;
    let mut by = Provenance::system();
    by.actor.clear();
    for (key, value) in entry.map() {
        let value = parse_scalar(&value);
        match key.as_str() {
            "kind" => {
                kind = match value.as_str() {
                    "person" => Some(ActorKind::Person),
                    "agent" => Some(ActorKind::Agent),
                    "system" => Some(ActorKind::System),
                    _ => return None,
                }
            }
            "actor" => by.actor = value,
            "session" => by.session = Some(value),
            "source" => by.source = Some(value),
            _ => {}
        }
    }
    by.kind = kind?;
    (!by.actor.trim().is_empty()).then_some(by)
}

/// Whether `kind` is a node kind: `[a-z][a-z0-9-]*`, at most 32 characters.
fn is_kind(kind: &str) -> bool {
    kind.len() <= 32
        && kind.starts_with(|c: char| c.is_ascii_lowercase())
        && kind
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub(crate) enum Outcome {
    Created,
    Updated,
    Unchanged,
}

/// The node a file `id` stands for: the node with that id, or a node imported from a file with
/// that id by an earlier version of Knode, which gave it an id of its own.
pub(crate) fn find(space: &Space, id: &str) -> Result<Option<NodeId>> {
    let Ok(id) = NodeId::parse(id) else {
        return Ok(None);
    };
    if space.get_node(&id)?.is_some() {
        return Ok(Some(id));
    }
    Ok(space
        .find_by_property(keys::ID, &id.to_string())?
        .first()
        .map(|s| s.id))
}

/// Writes one note to the space: updates the node it matches, or creates one.
fn write(
    space: &mut Space,
    rel: &str,
    note: &mut Note,
    by: &Provenance,
    hook: &mut Hook<'_>,
) -> Result<(NodeId, Outcome)> {
    let existing = match note.id {
        Some(id) => find(space, &id.to_string())?,
        None => space
            .find_by_property(keys::PATH, rel)?
            .first()
            .map(|s| s.id),
    };

    if let Some(id) = existing {
        hook(space, id, rel, note)?;
    }
    let note: &Note = note;

    let mut props = note.props.clone();
    props.insert(keys::PATH.into(), rel.into());
    if note.vault {
        props.insert(keys::STYLE.into(), STYLE_VAULT.into());
    }
    if !note.extra.is_empty() {
        props.insert(keys::FRONTMATTER.into(), note.extra.clone());
    }
    let mut stale: Vec<&str> = Vec::new();
    if !note.vault {
        stale.push(keys::STYLE);
    }
    if note.extra.is_empty() {
        stale.push(keys::FRONTMATTER);
    }
    let (id, outcome) = match existing {
        // A file from another space: the node keeps its id, and its dates and authors when
        // Knode wrote the file.
        None if note.id.is_some() => {
            let id = note.id.expect("checked above");
            let now = Timestamp::now();
            let read = |value: &Option<String>| value.as_deref().and_then(time::parse);
            let (created_at, updated_at, created_by, updated_by) = if note.vault {
                (now, now, by.clone(), by.clone())
            } else {
                let created_at = read(&note.created).unwrap_or(now);
                let updated_at = read(&note.updated).unwrap_or(created_at).max(created_at);
                let created_by = note.created_by.clone().unwrap_or_else(|| by.clone());
                let updated_by = match (&note.updated_by, &note.created_by) {
                    (Some(updated_by), _) => updated_by.clone(),
                    (None, Some(created_by)) => created_by.clone(),
                    (None, None) => by.clone(),
                };
                (created_at, updated_at, created_by, updated_by)
            };
            let node = space.import_node(
                ImportedNode {
                    id,
                    node: NewNode {
                        kind: note.kind.clone().unwrap_or_default(),
                        title: note.title.clone(),
                        body: note.body.clone(),
                        tags: note.tags.clone(),
                    },
                    created_at,
                    updated_at,
                    created_by,
                    updated_by,
                },
                &props,
                by,
            )?;
            (node.id, Outcome::Created)
        }
        None => {
            let node = space.create_node_with_properties(
                NewNode {
                    kind: note.kind.clone().unwrap_or_default(),
                    title: note.title.clone(),
                    body: note.body.clone(),
                    tags: note.tags.clone(),
                },
                &props,
                by,
            )?;
            (node.id, Outcome::Created)
        }
        Some(id) => {
            let before = space.get_node(&id)?.map(|n| n.updated_at);
            let node = space.update_node(
                &id,
                NodeUpdate {
                    kind: note.kind.clone(),
                    title: Some(note.title.clone()),
                    body: Some(note.body.clone()),
                    tags: Some(note.tags.clone()),
                },
                by,
            )?;
            let mut changed = before != Some(node.updated_at);
            changed |= space.set_properties(&id, &props, by)?;
            for key in stale {
                changed |= space.remove_property(&id, key)?;
            }
            let outcome = if changed {
                Outcome::Updated
            } else {
                Outcome::Unchanged
            };
            (id, outcome)
        }
    };
    Ok((id, outcome))
}
