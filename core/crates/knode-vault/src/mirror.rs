//! Writing a space to a folder of `.md` files: [`export`], and the engine [`LiveFolder`] uses.
//!
//! [`LiveFolder`]: crate::LiveFolder

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use knode_format::{LinkKindSpec, Node, NodeId, Properties, Provenance, Space, kinds, link_kinds};
use serde::{Deserialize, Serialize};

use crate::attachments::{blob_urls_to_files, is_exportable_name, is_safe_path, numbered};
use crate::frontmatter::{self, entries, flow_list, scalar};
use crate::names::{
    fold, folder_of, is_safe_relative, join, safe_stem, stem_fits, stem_of, title_from_stem,
};
use crate::property_keys as keys;
use crate::{Error, FILES_MAP, LINK_KINDS_FILE, Result, STATE_DIR, STYLE_VAULT, io_error, tags};

/// What [`export`] (or a [`LiveFolder`](crate::LiveFolder) sync) did to the folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportReport {
    /// The files written, relative to the folder, with `/` between folders.
    pub written: Vec<String>,
    /// How many files already had the right content and were left as they were.
    pub unchanged: usize,
    /// The files removed: files Knode had written for nodes that were deleted, renamed or moved.
    pub removed: Vec<String>,
    /// How many nodes the folder holds now.
    pub files: usize,
    /// How many attachment files the folder holds now.
    #[serde(default)]
    pub attachments: usize,
    /// The attachments not written, as `<note path>: <name>`, with why: a name a file cannot
    /// have in the folder (hidden, or a `.md` file), or a file missing from the space.
    #[serde(default)]
    pub left_out: Vec<String>,
}

/// Writes every node of `space` to the folder `dir` as a `.md` file, creating the folder if
/// needed. See the [crate documentation](crate) for the files written.
///
/// Exporting again to the same folder rewrites only the files whose content changed, keeps the
/// file name of every node whose title still fits it, and removes the files it wrote for nodes
/// that no longer exist or moved. Files Knode did not write are never changed or removed: a node
/// whose file name is taken by such a file is written to `Title 2.md`.
///
/// Exporting does not change the space.
pub fn export(space: &Space, dir: impl AsRef<Path>) -> Result<ExportReport> {
    Mirror::load(dir.as_ref())?.sync(space, None)
}

/// `.knode/files.json`.
#[derive(Debug, Serialize, Deserialize)]
struct FilesMap {
    version: u32,
    /// Node id, as written in the files, to path.
    files: BTreeMap<String, String>,
    /// Node id, as written in the files, to the attachments written for it, by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    attachments: BTreeMap<String, BTreeMap<String, AttachmentFile>>,
}

/// An attachment written to the folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AttachmentFile {
    /// The file, relative to the folder, with `/` between folders.
    pub(crate) path: String,
    /// The hash of its blob.
    pub(crate) hash: String,
}

/// Node id, as written in the files, to its attachment files by name.
pub(crate) type AttachmentMap = BTreeMap<String, BTreeMap<String, AttachmentFile>>;

/// `.knode/link-kinds.json`: the kinds of link the space defines.
#[derive(Debug, Serialize, Deserialize)]
struct LinkKindsFile {
    version: u32,
    kinds: Vec<LinkKindSpec>,
}

/// Reads `.knode/files.json` of `dir`: the paths of the nodes and of their attachments.
fn read_map(dir: &Path) -> Result<(BTreeMap<String, String>, AttachmentMap)> {
    let path = dir.join(STATE_DIR).join(FILES_MAP);
    match fs::read_to_string(&path) {
        Ok(text) => {
            let map: FilesMap = serde_json::from_str(&text).map_err(|e| Error::Map {
                path: path.clone(),
                message: e.to_string(),
            })?;
            let files = map
                .files
                .into_iter()
                .filter(|(_, p)| is_safe_relative(p))
                .collect();
            let mut attachments = map.attachments;
            for files in attachments.values_mut() {
                files.retain(|_, f| is_safe_path(&f.path));
            }
            Ok((files, attachments))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(e) => Err(io_error(&path)(e)),
    }
}

/// The attachment files `.knode/files.json` of `dir` lists, or none when it has none or cannot
/// be read.
pub(crate) fn read_attachment_map(dir: &Path) -> AttachmentMap {
    read_map(dir).map(|(_, a)| a).unwrap_or_default()
}

/// The kinds of link `.knode/link-kinds.json` of `dir` defines, by name, or none when it has
/// none or cannot be read.
pub(crate) fn read_link_kinds(dir: &Path) -> BTreeMap<String, LinkKindSpec> {
    fs::read_to_string(dir.join(STATE_DIR).join(LINK_KINDS_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<LinkKindsFile>(&text).ok())
        .map(|file| {
            file.kinds
                .into_iter()
                .map(|k| (k.kind.clone(), k))
                .collect()
        })
        .unwrap_or_default()
}

/// A folder that mirrors a space, with the path of each node.
#[derive(Debug)]
pub(crate) struct Mirror {
    dir: PathBuf,
    /// Export id to path, as last written.
    files: BTreeMap<String, String>,
    /// Export id to attachment files, as last written.
    attachments: AttachmentMap,
}

/// A node of the space, as the mirror sees it.
struct Item {
    id: NodeId,
    title: String,
    /// The id written in the file: [`keys::ID`] if set, else the node id.
    export_id: String,
    props: Properties,
}

impl Mirror {
    /// Opens the folder `dir`, reading `.knode/files.json` if it has one.
    pub(crate) fn load(dir: &Path) -> Result<Mirror> {
        let (files, attachments) = read_map(dir)?;
        Ok(Mirror {
            dir: dir.to_path_buf(),
            files,
            attachments,
        })
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Brings the folder in step with the space. With `only`, writes just those nodes, the nodes
    /// whose `[[links]]` point at them, and every node whose path changed; without it, writes
    /// every node. Files of nodes that are gone or moved are removed either way.
    pub(crate) fn sync(
        &mut self,
        space: &Space,
        only: Option<&BTreeSet<NodeId>>,
    ) -> Result<ExportReport> {
        let items = index(space)?;
        let assigned = self.assign(&items);
        let export_ids: HashMap<NodeId, String> =
            items.iter().map(|i| (i.id, i.export_id.clone())).collect();

        let write: HashSet<&str> = match only {
            None => items.iter().map(|i| i.export_id.as_str()).collect(),
            Some(ids) => {
                let mut set = HashSet::new();
                for id in ids {
                    if let Some(eid) = export_ids.get(id) {
                        set.insert(eid.as_str());
                    }
                    for link in space.backlinks(id)? {
                        if link.kind == link_kinds::MENTIONS {
                            if let Some(eid) = export_ids.get(&link.from) {
                                set.insert(eid.as_str());
                            }
                        }
                    }
                }
                for item in &items {
                    if self.files.get(&item.export_id) != assigned.get(&item.export_id) {
                        set.insert(item.export_id.as_str());
                    }
                }
                set
            }
        };

        let mut report = ExportReport::default();
        let attached = self.assign_attachments(space, &items, &assigned, &mut report)?;

        // Remove first, so a path freed by one node can be taken by another.
        let kept: HashSet<String> = assigned.values().map(|p| fold(p)).collect();
        for (eid, path) in &self.files {
            if assigned.get(eid) == Some(path) || kept.contains(&fold(path)) {
                continue;
            }
            let full = self.dir.join(path);
            if read_id(&full).as_deref() == Some(eid.as_str()) {
                fs::remove_file(&full).map_err(io_error(&full))?;
                report.removed.push(path.clone());
            }
        }

        for item in &items {
            if !write.contains(item.export_id.as_str()) {
                continue;
            }
            let path = &assigned[&item.export_id];
            let Some(node) = space.get_node(&item.id)? else {
                continue;
            };
            // The file name of each attachment, by hash, for `knode:blob/<hash>` links.
            let mut blob_files = BTreeMap::new();
            for file in attached
                .get(&item.export_id)
                .into_iter()
                .flat_map(|a| a.values())
            {
                let name = file.path.rsplit('/').next().unwrap_or(&file.path);
                blob_files
                    .entry(file.hash.clone())
                    .or_insert_with(|| name.to_string());
            }
            let text = render(space, &node, item, &export_ids, stem_of(path), &blob_files)?;
            if write_if_changed(&self.dir.join(path), text.as_bytes())? {
                report.written.push(path.clone());
            } else {
                report.unchanged += 1;
            }
        }

        self.write_attachments(space, &attached, &assigned, &mut report)?;
        self.save_link_kinds(space)?;

        report.files = assigned.len();
        self.files = assigned;
        self.attachments = attached;
        self.save()?;
        Ok(report)
    }

    /// Chooses the file of every attachment of every node: next to the node's file, under its
    /// name, or ` 2`, ` 3`... before the extension when another node's attachment with other
    /// content, a note or a file Knode did not write has that name. A file it had before is kept.
    fn assign_attachments(
        &self,
        space: &Space,
        items: &[Item],
        assigned: &BTreeMap<String, String>,
        report: &mut ExportReport,
    ) -> Result<AttachmentMap> {
        let notes: HashSet<String> = assigned.values().map(|p| fold(p)).collect();
        let ours: HashSet<String> = self
            .attachments
            .values()
            .flat_map(|a| a.values())
            .map(|f| fold(&f.path))
            .collect();
        let mut claimed: HashMap<String, String> = HashMap::new();
        let mut out = AttachmentMap::new();
        for item in items {
            let attachments = space.attachments(&item.id)?;
            if attachments.is_empty() {
                continue;
            }
            let note = &assigned[&item.export_id];
            let folder = folder_of(note);
            let previous = self.attachments.get(&item.export_id);
            for attachment in attachments {
                let name = &attachment.name;
                if !is_exportable_name(name) {
                    report
                        .left_out
                        .push(format!("{note}: {name} (not a name for a file here)"));
                    continue;
                }
                let mut blob: Option<Option<Vec<u8>>> = None;
                let mut free = |path: &str| -> Result<bool> {
                    let key = fold(path);
                    if notes.contains(&key) {
                        return Ok(false);
                    }
                    if let Some(hash) = claimed.get(&key) {
                        return Ok(*hash == attachment.hash);
                    }
                    let full = self.dir.join(path);
                    let Ok(meta) = fs::symlink_metadata(&full) else {
                        return Ok(true);
                    };
                    if !meta.is_file() {
                        return Ok(false);
                    }
                    if ours.contains(&key) {
                        return Ok(true);
                    }
                    // A file Knode did not write: shared only when it is this very file.
                    if blob.is_none() {
                        blob = Some(space.read_blob(&attachment.hash).ok().flatten());
                    }
                    let same = match blob.as_ref().and_then(|b| b.as_ref()) {
                        Some(bytes) => fs::read(&full).is_ok_and(|on_disk| on_disk == *bytes),
                        None => false,
                    };
                    Ok(same)
                };
                let mut chosen = None;
                if let Some(before) = previous.and_then(|p| p.get(name)) {
                    if before.hash == attachment.hash
                        && folder_of(&before.path) == folder
                        && free(&before.path)?
                    {
                        chosen = Some(before.path.clone());
                    }
                }
                let mut n = 1;
                while chosen.is_none() {
                    let path = join(folder, &numbered(name, n));
                    if free(&path)? {
                        chosen = Some(path);
                    }
                    n += 1;
                }
                let path = chosen.expect("chosen above");
                claimed.insert(fold(&path), attachment.hash.clone());
                out.entry(item.export_id.clone()).or_default().insert(
                    name.clone(),
                    AttachmentFile {
                        path,
                        hash: attachment.hash,
                    },
                );
            }
        }
        Ok(out)
    }

    /// Writes the attachment files that are not on disk yet, and removes the files written
    /// before for attachments that are gone, as long as they still hold what Knode wrote.
    fn write_attachments(
        &self,
        space: &Space,
        attached: &AttachmentMap,
        assigned: &BTreeMap<String, String>,
        report: &mut ExportReport,
    ) -> Result<()> {
        let before: HashMap<&str, &str> = self
            .attachments
            .values()
            .flat_map(|a| a.values())
            .map(|f| (f.path.as_str(), f.hash.as_str()))
            .collect();
        let mut done: HashSet<String> = HashSet::new();
        let mut kept: HashSet<String> = HashSet::new();
        for (eid, files) in attached {
            for (name, file) in files {
                if !done.insert(fold(&file.path)) {
                    kept.insert(fold(&file.path));
                    continue;
                }
                let full = self.dir.join(&file.path);
                if before.get(file.path.as_str()) == Some(&file.hash.as_str()) && full.is_file() {
                    kept.insert(fold(&file.path));
                    report.unchanged += 1;
                    continue;
                }
                let bytes = match space.read_blob(&file.hash) {
                    Ok(Some(bytes)) => bytes,
                    Ok(None) | Err(_) => {
                        let note = assigned.get(eid).map(String::as_str).unwrap_or(eid);
                        report.left_out.push(format!(
                            "{note}: {name} (its file is missing from the space)"
                        ));
                        done.remove(&fold(&file.path));
                        continue;
                    }
                };
                kept.insert(fold(&file.path));
                if write_if_changed(&full, &bytes)? {
                    report.written.push(file.path.clone());
                } else {
                    report.unchanged += 1;
                }
            }
        }
        report.attachments = kept.len();
        for (path, hash) in before {
            if kept.contains(&fold(path)) {
                continue;
            }
            let full = self.dir.join(path);
            let ours = match space.read_blob(hash) {
                Ok(Some(bytes)) => fs::read(&full).is_ok_and(|on_disk| on_disk == bytes),
                _ => false,
            };
            if ours {
                fs::remove_file(&full).map_err(io_error(&full))?;
                report.removed.push(path.to_string());
            }
        }
        Ok(())
    }

    /// Writes `.knode/link-kinds.json` with the kinds of link the space defines, or removes it
    /// when the space defines none.
    fn save_link_kinds(&self, space: &Space) -> Result<()> {
        let path = self.dir.join(STATE_DIR).join(LINK_KINDS_FILE);
        let kinds: Vec<LinkKindSpec> = space
            .link_kinds()?
            .into_iter()
            .filter(|k| !k.built_in)
            .map(|k| LinkKindSpec {
                kind: k.kind,
                label: k.label,
                inverse_label: k.inverse_label,
                direction: k.direction,
                description: k.description,
            })
            .collect();
        if kinds.is_empty() {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_error(&path)(e)),
            }
            return Ok(());
        }
        let file = LinkKindsFile { version: 1, kinds };
        let mut text = serde_json::to_string_pretty(&file).map_err(|e| Error::Map {
            path: path.clone(),
            message: e.to_string(),
        })?;
        text.push('\n');
        write_if_changed(&path, text.as_bytes())?;
        Ok(())
    }

    /// Writes `.knode/files.json` if it changed.
    fn save(&self) -> Result<()> {
        let map = FilesMap {
            version: 1,
            files: self.files.clone(),
            attachments: self.attachments.clone(),
        };
        let mut text = serde_json::to_string_pretty(&map).map_err(|e| Error::Map {
            path: self.dir.join(STATE_DIR).join(FILES_MAP),
            message: e.to_string(),
        })?;
        text.push('\n');
        write_if_changed(&self.dir.join(STATE_DIR).join(FILES_MAP), text.as_bytes())?;
        Ok(())
    }

    /// Chooses the path of every node: the path it was written to before, else the path it was
    /// imported from, else one made from its title, as long as the title still fits the name and
    /// no other node or foreign file has it. Nodes are placed in the order of their ids.
    fn assign(&self, items: &[Item]) -> BTreeMap<String, String> {
        let owners: HashMap<String, &str> = self
            .files
            .iter()
            .map(|(eid, path)| (fold(path), eid.as_str()))
            .collect();
        let mut claimed: HashSet<String> = HashSet::new();
        let mut out: BTreeMap<String, String> = BTreeMap::new();

        let folder = |item: &Item| -> String {
            item.props
                .get(keys::PATH)
                .filter(|p| is_safe_relative(p))
                .map(|p| folder_of(p).to_string())
                .unwrap_or_default()
        };
        let mut try_claim = |item: &Item, path: &str, out: &mut BTreeMap<String, String>| {
            let key = fold(path);
            if claimed.contains(&key) || self.occupied(path, &item.export_id, &owners) {
                return false;
            }
            claimed.insert(key);
            out.insert(item.export_id.clone(), path.to_string());
            true
        };

        // The path each node had, then the path it came from.
        let previous = |item: &Item| self.files.get(&item.export_id).cloned();
        let imported = |item: &Item| item.props.get(keys::PATH).cloned();
        for choose in [&previous as &dyn Fn(&Item) -> Option<String>, &imported] {
            for item in items {
                if out.contains_key(&item.export_id) {
                    continue;
                }
                if let Some(path) = choose(item) {
                    if is_safe_relative(&path)
                        && folder_of(&path) == folder(item)
                        && stem_fits(stem_of(&path), &item.title)
                    {
                        try_claim(item, &path, &mut out);
                    }
                }
            }
        }

        // A name made from the title.
        for item in items {
            if out.contains_key(&item.export_id) {
                continue;
            }
            let folder = folder(item);
            let stem = safe_stem(&item.title);
            let mut n = 1u32;
            loop {
                let name = if n == 1 {
                    format!("{stem}.md")
                } else {
                    format!("{stem} {n}.md")
                };
                if try_claim(item, &join(&folder, &name), &mut out) {
                    break;
                }
                n += 1;
            }
        }
        out
    }

    /// Whether `path` holds a file Knode must not overwrite for the node `export_id`: anything
    /// but nothing, a file of this node, or a file Knode wrote for another node.
    fn occupied(&self, path: &str, export_id: &str, owners: &HashMap<String, &str>) -> bool {
        let full = self.dir.join(path);
        let Ok(meta) = fs::symlink_metadata(&full) else {
            return false;
        };
        if !meta.is_file() {
            return true;
        }
        let on_disk = read_id(&full);
        if on_disk.as_deref() == Some(export_id) {
            return false;
        }
        match owners.get(&fold(path)) {
            Some(owner) => on_disk.as_deref() != Some(*owner),
            None => true,
        }
    }
}

/// Every node of the space with its properties and export id, in the order of export ids.
fn index(space: &Space) -> Result<Vec<Item>> {
    let mut items = Vec::new();
    for summary in space.recent(usize::MAX)? {
        let props = space.properties(&summary.id)?;
        let export_id = props
            .get(keys::ID)
            .and_then(|v| NodeId::parse(v).ok())
            .unwrap_or(summary.id)
            .to_string();
        items.push(Item {
            id: summary.id,
            title: summary.title,
            export_id,
            props,
        });
    }
    items.sort_by(|a, b| a.export_id.cmp(&b.export_id).then(a.id.cmp(&b.id)));
    // Two nodes cannot share an id in one folder: the later one is written with its own.
    let mut seen = HashSet::new();
    for item in &mut items {
        if !seen.insert(item.export_id.clone()) {
            item.export_id = item.id.to_string();
            seen.insert(item.export_id.clone());
        }
    }
    items.sort_by(|a, b| a.export_id.cmp(&b.export_id));
    Ok(items)
}

/// The text of a node's file, named `stem`.
fn render(
    space: &Space,
    node: &Node,
    item: &Item,
    export_ids: &HashMap<NodeId, String>,
    stem: &str,
    blob_files: &BTreeMap<String, String>,
) -> Result<String> {
    let mut links: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for link in space.links_from(&node.id)? {
        if link.kind == link_kinds::MENTIONS {
            continue;
        }
        let to = export_ids
            .get(&link.to)
            .cloned()
            .unwrap_or_else(|| link.to.to_string());
        links.entry(link.kind).or_default().insert(to);
    }
    let props: Vec<(&String, &String)> = item
        .props
        .iter()
        .filter(|(k, _)| !k.starts_with(keys::PREFIX))
        .collect();
    let extra = item
        .props
        .get(keys::FRONTMATTER)
        .map(String::as_str)
        .unwrap_or("");
    let vault = item.props.get(keys::STYLE).map(String::as_str) == Some(STYLE_VAULT);

    let nl =
        if vault && (extra.contains("\r\n") || (extra.is_empty() && first_line_crlf(&node.body))) {
            "\r\n"
        } else {
            "\n"
        };
    let mut fm = String::new();
    let mut line = |key: &str, value: &str| {
        fm.push_str(key);
        fm.push(':');
        if !value.is_empty() {
            fm.push(' ');
            fm.push_str(value);
        }
        fm.push_str(nl);
    };
    line("id", &item.export_id);
    if title_from_stem(stem) != node.title {
        line("title", &scalar(&node.title));
    }
    if !vault || node.kind != kinds::NOTE {
        line("kind", &scalar(&node.kind));
    }
    if !vault {
        if !node.tags.is_empty() {
            line("tags", &flow_list(&node.tags));
        }
        let created = item
            .props
            .get(keys::CREATED)
            .filter(|_| item.props.contains_key(keys::ID))
            .filter(|v| crate::time::parse(v).is_some())
            .cloned()
            .unwrap_or_else(|| node.created_at.to_string());
        let updated_at = node.updated_at.to_string();
        let updated = item
            .props
            .get(keys::UPDATED)
            .filter(|_| item.props.contains_key(keys::ID))
            .filter(|_| item.props.get(keys::WRITTEN) == Some(&updated_at))
            .filter(|v| crate::time::parse(v).is_some())
            .cloned()
            .unwrap_or(updated_at);
        line("created", &created);
        provenance(&mut line, "created_by", &node.created_by);
        line("updated", &updated);
        if node.updated_by != node.created_by {
            provenance(&mut line, "updated_by", &node.updated_by);
        }
    }
    if !links.is_empty() {
        line("links", "");
        for (kind, targets) in &links {
            let targets: Vec<&String> = targets.iter().collect();
            line(&format!("  {}", scalar(kind)), &flow_list(&targets));
        }
    }
    if !props.is_empty() {
        line("properties", "");
        for (key, value) in props {
            line(&format!("  {}", scalar(key)), &scalar(value));
        }
    }
    if vault {
        fm.push_str(&vault_frontmatter(extra, node, nl));
    } else if !extra.is_empty() {
        fm.push_str(extra);
        if !extra.ends_with('\n') {
            fm.push_str(nl);
        }
    }
    let body = blob_urls_to_files(&node.body, blob_files);
    Ok(format!("---{nl}{fm}---{nl}{body}"))
}

/// Writes a provenance as `kind`, `actor`, `session` and `source` pairs under `key`.
fn provenance(line: &mut impl FnMut(&str, &str), key: &str, by: &Provenance) {
    line(key, "");
    line("  kind", by.kind.as_str());
    line("  actor", &scalar(&by.actor));
    if let Some(session) = &by.session {
        line("  session", &scalar(session));
    }
    if let Some(source) = &by.source {
        line("  source", &scalar(source));
    }
}

/// The frontmatter of a note from another app, as it was imported, with its tags changed only if
/// the node's tags no longer match those of the frontmatter and the inline `#tags`.
fn vault_frontmatter(extra: &str, node: &Node, nl: &str) -> String {
    let entries = entries(extra);
    let is_tags = |e: &frontmatter::Entry<'_>| matches!(e.key.as_deref(), Some("tags" | "tag"));
    let written: BTreeSet<String> = entries
        .iter()
        .filter(|e| is_tags(e))
        .flat_map(|e| e.list())
        .filter_map(|t| tags::clean(&t))
        .collect();
    let inline = tags::inline(&node.body);
    let implied: BTreeSet<String> = written.union(&inline).cloned().collect();
    let current: BTreeSet<String> = node.tags.iter().cloned().collect();
    if implied == current {
        return extra.to_string();
    }
    let want: Vec<&String> = current.difference(&inline).collect();
    let tags_line = format!("tags: {}{nl}", flow_list(&want));
    let mut out = String::new();
    let mut placed = false;
    for entry in &entries {
        if is_tags(entry) {
            if !placed && !want.is_empty() {
                out.push_str(&tags_line);
            }
            placed = true;
        } else {
            out.push_str(entry.text);
        }
    }
    if !placed && !want.is_empty() {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(nl);
        }
        out.push_str(&tags_line);
    }
    out
}

fn first_line_crlf(text: &str) -> bool {
    frontmatter::split(text).newline == "\r\n"
}

/// The id in the frontmatter of the file at `path`, in upper case, if it has one.
pub(crate) fn read_id(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let fm = frontmatter::split(&text).frontmatter?;
    let entry = entries(fm)
        .into_iter()
        .find(|e| e.key.as_deref() == Some("id"))?;
    NodeId::parse(&entry.scalar()?)
        .ok()
        .map(|id| id.to_string())
}

/// Writes `content` to `path` through a temporary file in the same folder, unless the file
/// already has exactly this content. Returns whether it wrote.
pub(crate) fn write_if_changed(path: &Path, content: &[u8]) -> Result<bool> {
    if fs::read(path).is_ok_and(|current| current == content) {
        return Ok(false);
    }
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(io_error(parent))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = parent.join(format!(".{name}.knode-tmp"));
    fs::write(&tmp, content).map_err(io_error(&tmp))?;
    fs::rename(&tmp, path).map_err(io_error(path))?;
    Ok(true)
}
