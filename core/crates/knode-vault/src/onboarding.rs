//! Reviewable, deterministic project onboarding into a new independent space.
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use knode_format::{Manifest, NewNode, Provenance, Space};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Error, ImportReport, Result, io_error};

/// How the person currently works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// A Markdown/Obsidian memory containing several projects.
    Vault,
    /// A single repository/project with agent instructions and documentation.
    Project,
    /// Start with a small empty project context.
    Empty,
}
/// A suggested group. Its stable id is separate from the editable display name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    /// Stable grouping key within this plan.
    pub id: String,
    /// Editable project name; equal names deliberately merge groups on import.
    pub name: String,
}
/// A source document whose inclusion and project assignment may be edited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    /// Relative source path.
    pub path: String,
    /// Parsed title.
    pub title: String,
    /// Group id, referring to [`Plan::groups`].
    pub project: String,
    /// Whether to import this document.
    pub include: bool,
    /// Source digest; revalidated before writing.
    pub digest: String,
}
/// An immutable fingerprint of an eligible source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFile {
    /// Relative path.
    pub path: String,
    /// SHA-256 of the source bytes.
    pub digest: String,
}
/// A reviewable proposal. No source content is modified by analysis or import.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    /// Plan schema version.
    pub version: u32,
    /// Source mode.
    pub mode: Mode,
    /// Canonical source folder, absent for an empty start.
    pub source: Option<PathBuf>,
    /// Editable groups, including shared material if present.
    pub groups: Vec<Group>,
    /// Editable assignments and exclusions.
    pub documents: Vec<Document>,
    /// Source snapshot identity, not an atomic filesystem snapshot.
    pub files: Vec<SourceFile>,
    /// Ignored paths, potential incompatibilities and interpretation limits.
    pub warnings: Vec<String>,
}
/// Evidence saved in a new onboarding space; also guards its archive action.
#[derive(Debug, Serialize, Deserialize)]
pub struct Receipt {
    /// Identity of the newly created space.
    pub space_id: String,
    /// Path of the independent imported space.
    pub destination: PathBuf,
    /// The reviewed proposal.
    pub plan: Plan,
    /// The actual importer result.
    pub import: ImportReport,
    /// Generated context note IDs, keyed by reviewed project name.
    pub contexts: BTreeMap<String, String>,
    /// Unresolved links after import, keyed by source path/title.
    pub unresolved: BTreeMap<String, Vec<String>>,
    /// Source attachment references normalized to node-local names or blob URLs.
    #[serde(default)]
    pub attachment_links: Vec<AttachmentLink>,
}
/// A reviewable attachment-link adaptation; original source files remain unchanged.
#[derive(Debug, Serialize, Deserialize)]
pub struct AttachmentLink {
    /// Original document path.
    pub document: String,
    /// Original attachment path inside the source.
    pub source: String,
    /// Actual attachment name stored on this note.
    pub name: String,
}
const RECEIPT: &str = "onboarding.json";
const OMIT: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    "Pods",
    "__pycache__",
    "venv",
    "coverage",
];
fn invalid(message: impl Into<String>) -> Error {
    Error::Map {
        path: PathBuf::from("onboarding"),
        message: message.into(),
    }
}
fn safe(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        && path.split('/').all(|s| !s.starts_with('.'))
}
fn digest(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(io_error(path))?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf).map_err(io_error(path))?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn inventory(
    root: &Path,
    prefix: &str,
    files: &mut Vec<SourceFile>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    for entry in fs::read_dir(root).map_err(io_error(root))? {
        let entry = entry.map_err(io_error(root))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            warnings.push("A non-UTF-8 filename was excluded.".into());
            continue;
        };
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let meta = fs::symlink_metadata(entry.path()).map_err(io_error(entry.path()))?;
        if name.starts_with('.')
            || OMIT.contains(&name.as_str())
            || meta.file_type().is_symlink()
            || (meta.is_dir() && name.ends_with(".knode"))
        {
            warnings.push(format!(
                "Excluded: {rel} (hidden, generated/dependency folder, symlink or nested space)."
            ));
            continue;
        }
        if meta.is_dir() {
            inventory(&entry.path(), &rel, files, warnings)?;
        } else if meta.is_file() {
            if meta.len() > knode_format::MAX_ATTACHMENT_SIZE {
                warnings.push(format!("Excluded oversized file: {rel}."));
                continue;
            }
            if ["pem", "key", "p12", "pfx"]
                .iter()
                .any(|ext| name.ends_with(&format!(".{ext}")))
            {
                warnings.push(format!("Excluded possible credential file: {rel}."));
                continue;
            }
            if name.ends_with(".canvas") {
                warnings.push(format!(
                    "{rel}: Obsidian canvas layout is not converted; review it separately."
                ));
            }
            files.push(SourceFile {
                path: rel,
                digest: digest(&entry.path())?,
            });
        }
    }
    Ok(())
}
/// Analyze locally. Project guesses follow folders, not inferred meanings or decisions.
pub fn analyze(source: Option<&Path>, mode: Mode) -> Result<Plan> {
    let source = match (source, mode) {
        (None, Mode::Empty) => None,
        (Some(path), m) if m != Mode::Empty => Some(path.canonicalize().map_err(io_error(path))?),
        _ => return Err(invalid("Choose a source folder, or choose an empty start.")),
    };
    let mut plan=Plan {version:1,mode,source,groups:vec![],documents:vec![],files:vec![],warnings:vec!["Folder-based suggestions only: no AI summaries or inferred decisions. Review assignments. Sources stay unchanged; changes in Knode are not synchronized back. Plugin behavior is not migrated. Review documents for private information before connecting an external agent.".into()]};
    if let Some(root) = &plan.source {
        inventory(root, "", &mut plan.files, &mut plan.warnings)?;
        plan.files.sort_by(|a, b| a.path.cmp(&b.path));
        let mut groups = BTreeMap::new();
        for file in &plan.files {
            if !file.path.ends_with(".md") {
                continue;
            }
            let text = match fs::read_to_string(root.join(&file.path)) {
                Ok(t) => t,
                Err(e) => {
                    plan.warnings.push(format!(
                        "{}: not imported as UTF-8 Markdown ({e}).",
                        file.path
                    ));
                    continue;
                }
            };
            let note = crate::import::parse(&file.path, &text);
            let id = if mode == Mode::Project {
                "project".to_owned()
            } else {
                file.path
                    .split_once('/')
                    .map_or("shared", |(first, _)| first)
                    .to_owned()
            };
            let id = if mode == Mode::Vault
                && ["Projects", "projects", "Proyectos", "proyectos"].contains(&id.as_str())
            {
                file.path
                    .rsplit_once('/')
                    .and_then(|(parent, _)| parent.split_once('/'))
                    .map_or(id.clone(), |(top, project)| {
                        format!("{top}/{}", project.split('/').next().unwrap_or(project))
                    })
            } else {
                id
            };
            let name = if id == "project" {
                root.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            } else if id == "shared" {
                "Shared context".into()
            } else {
                id.rsplit('/').next().unwrap_or(&id).to_owned()
            };
            groups.insert(id.clone(), name);
            if text.contains("```dataview") || text.contains("```tasks") || text.contains("<%") {
                plan.warnings.push(format!(
                    "{}: possible plugin/template code is preserved as text, not executed.",
                    file.path
                ));
            }
            for link in knode_md::wiki_links(&note.body) {
                if link.destination.contains('/')
                    && !link.target.ends_with(".md")
                    && Path::new(&link.target).extension().is_some()
                {
                    plan.warnings.push(format!("{}: review attachment path {}. Missing or ambiguous source files are left for review.",file.path,link.destination));
                }
            }
            plan.documents.push(Document {
                path: file.path.clone(),
                title: note.title,
                project: id,
                include: true,
                digest: file.digest.clone(),
            });
        }
        plan.groups = groups
            .into_iter()
            .map(|(id, name)| Group { id, name })
            .collect();
        if mode == Mode::Project && plan.groups.is_empty() {
            plan.groups.push(Group {
                id: "project".into(),
                name: root
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            });
        }
    } else {
        plan.groups.push(Group {
            id: "project".into(),
            name: "My project".into(),
        });
    }
    plan.warnings.sort();
    plan.warnings.dedup();
    Ok(plan)
}
/// Create an independent space after validating the reviewed plan against the current source.
/// Existing destinations and destinations inside the source are refused. No agents are connected.
pub fn apply(plan: &Plan, destination: &Path, name: &str, by: &Provenance) -> Result<Receipt> {
    if plan.version != 1
        || name.trim().is_empty()
        || destination.extension().and_then(|s| s.to_str()) != Some("knode")
    {
        return Err(invalid(
            "A version-1 plan, a space name and a new .knode destination are required.",
        ));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| invalid("Choose a destination folder."))?
        .canonicalize()
        .map_err(io_error(destination))?;
    let destination = parent.join(
        destination
            .file_name()
            .ok_or_else(|| invalid("Invalid destination."))?,
    );
    if fs::symlink_metadata(&destination).is_ok() {
        return Err(invalid(
            "Destination already exists. Choose a new space; existing spaces are never overwritten.",
        ));
    }
    let groups: BTreeMap<_, _> = plan
        .groups
        .iter()
        .map(|g| (g.id.clone(), g.name.trim().to_owned()))
        .collect();
    if groups.len() != plan.groups.len()
        || groups.is_empty()
        || groups.values().any(|n| n.is_empty())
    {
        return Err(invalid(
            "Every group needs a unique id and a nonempty name.",
        ));
    }
    let mut paths = BTreeSet::new();
    for doc in &plan.documents {
        if !safe(&doc.path) || !paths.insert(doc.path.clone()) || !groups.contains_key(&doc.project)
        {
            return Err(invalid(
                "Invalid document path, duplicate document or missing project assignment.",
            ));
        }
    }
    if let Some(source) = &plan.source {
        if parent.starts_with(source) {
            return Err(invalid("Choose a destination outside the source folder."));
        }
        let current = analyze(Some(source), plan.mode)?;
        if current.files != plan.files || current.documents.len() != plan.documents.len() {
            return Err(invalid(
                "Source changed since analysis. Analyze again before importing.",
            ));
        }
        for doc in &plan.documents {
            if !current
                .documents
                .iter()
                .any(|n| n.path == doc.path && n.digest == doc.digest)
            {
                return Err(invalid("Source document plan changed. Analyze again."));
            }
        }
    } else if plan.mode != Mode::Empty || !plan.documents.is_empty() || !plan.files.is_empty() {
        return Err(invalid("Invalid empty-start plan."));
    }
    let staging = tempfile::Builder::new()
        .prefix(".knode-onboarding-")
        .tempdir_in(&parent)
        .map_err(io_error(&parent))?;
    let input = staging.path().join("input");
    fs::create_dir(&input).map_err(io_error(&input))?;
    if let Some(source) = &plan.source {
        let selected: BTreeSet<_> = plan
            .documents
            .iter()
            .filter(|n| n.include)
            .map(|n| n.path.as_str())
            .collect();
        for file in &plan.files {
            if !safe(&file.path) {
                return Err(invalid("Unsafe source path."));
            }
            if file.path.ends_with(".md") && !selected.contains(file.path.as_str()) {
                continue;
            }
            let source_file = source.join(&file.path);
            if fs::symlink_metadata(&source_file)
                .map_err(io_error(&source_file))?
                .file_type()
                .is_symlink()
                || digest(&source_file)? != file.digest
            {
                return Err(invalid("Source changed during import. Analyze again."));
            }
            let out = input.join(&file.path);
            fs::create_dir_all(out.parent().unwrap()).map_err(io_error(&out))?;
            fs::copy(&source_file, &out).map_err(io_error(&source_file))?;
            if digest(&out)? != file.digest {
                return Err(invalid("Source changed while copying. Analyze again."));
            }
        }
    }
    if let Some(source) = &plan.source
        && analyze(Some(source), plan.mode)?.files != plan.files
    {
        return Err(invalid(
            "Source changed while copying. Analyze again; no space was published.",
        ));
    }
    let staged = staging.path().join("Import.knode");
    let mut space = Space::create(&staged, name)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o700))
            .map_err(io_error(&staged))?;
    }
    space.set_setting("capture.enabled", Some("false"), by)?;
    let mut import = crate::import(&mut space, &input, by)?;
    let attachment_links = attach_source_references(&mut space, &input, plan, &mut import, by)?;
    if !import.skipped.is_empty() {
        return Err(invalid(format!(
            "Import could not preserve {} items. Nothing was published. Review source warnings and excluded documents.",
            import.skipped.len()
        )));
    }
    let nodes = space.recent(usize::try_from(space.node_count()?).unwrap_or(usize::MAX))?;
    if nodes.len() != plan.documents.iter().filter(|d| d.include).count() {
        return Err(invalid(
            "Source note IDs would merge documents. Nothing was published; exclude duplicates or repair their IDs before importing.",
        ));
    }
    let mut contexts = BTreeMap::new();
    let mut unresolved = BTreeMap::new();
    let names: BTreeSet<_> = groups.values().cloned().collect();
    for project in names {
        let members: Vec<_> = plan
            .documents
            .iter()
            .filter(|d| d.include && groups[&d.project] == project)
            .collect();
        if members.is_empty() && plan.mode == Mode::Vault {
            continue;
        }
        let mut title = format!("Project context — {project}");
        let mut suffix = 2;
        while !space.find_by_title(&title)?.is_empty() {
            title = format!("Project context — {project} ({suffix})");
            suffix += 1;
        }
        let mut body = format!(
            "# {project}\n\nImported context index. Review and edit this note before adopting the workflow.\n\nGoal: To be filled in.\nCurrent state: Not verified against the project.\nDecisions: No decisions inferred automatically.\nNext action: Review the source notes and record the actual next step.\n\n{} imported documents are linked to this context as part-of. The original source folder is unchanged.\n",
            members.len()
        );
        body.push_str("\n## Source notes\n\n");
        for member in members.iter().take(8) {
            for node in space.find_by_property(crate::property_keys::PATH, &member.path)? {
                body.push_str(&format!(
                    "- [[{}|{}]] — `{}`\n",
                    node.id,
                    node.title.replace([']', '|'], " "),
                    member.path.replace('`', " ")
                ));
            }
        }
        if members.len() > 8 {
            body.push_str("\nThis is a short source index. Other documents remain linked as part-of; search their original paths when needed.\n");
        }
        let context = space.create_node(NewNode::note(&title, &body), by)?;
        space.set_property(&context.id, "onboarding.generated", "project-context", by)?;
        for member in members {
            let found = space.find_by_property(crate::property_keys::PATH, &member.path)?;
            for node in found {
                space.link(&node.id, &context.id, "part-of", by)?;
            }
        }
        contexts.insert(project, context.id.to_string());
    }
    for node in nodes {
        let pending = space.unresolved_links(&node.id)?;
        if !pending.is_empty() {
            unresolved.insert(
                space
                    .property(&node.id, crate::property_keys::PATH)?
                    .unwrap_or(node.title),
                pending,
            );
        }
    }
    let receipt = Receipt {
        space_id: space.manifest().id.clone(),
        destination: destination.clone(),
        plan: plan.clone(),
        import,
        contexts,
        unresolved,
        attachment_links,
    };
    fs::write(
        staged.join(RECEIPT),
        serde_json::to_vec_pretty(&receipt).map_err(|e| invalid(e.to_string()))?,
    )
    .map_err(io_error(&staged))?;
    drop(space);
    // Reserve the name atomically; rename only replaces this empty reservation. On failure,
    // remove_dir never removes someone else's nonempty directory.
    fs::create_dir(&destination).map_err(io_error(&destination))?;
    if let Err(e) = fs::rename(&staged, &destination) {
        let _ = fs::remove_dir(&destination);
        return Err(io_error(&destination)(e));
    }
    Ok(receipt)
}
fn relative_asset(parent: &str, target: &str) -> Option<String> {
    if target.starts_with('/') || target.contains('\\') {
        return None;
    }
    let joined = if parent.is_empty() {
        target.to_owned()
    } else {
        format!("{parent}/{target}")
    };
    let mut parts = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            value => parts.push(value),
        }
    }
    let path = parts.join("/");
    safe(&path).then_some(path)
}
fn attach_source_references(
    space: &mut Space,
    input: &Path,
    plan: &Plan,
    report: &mut ImportReport,
    by: &Provenance,
) -> Result<Vec<AttachmentLink>> {
    use pulldown_cmark::{Event, LinkType, Parser, Tag};
    let assets: BTreeSet<_> = plan
        .files
        .iter()
        .filter(|f| !f.path.to_lowercase().ends_with(".md"))
        .map(|f| f.path.as_str())
        .collect();
    let mut adapted = Vec::new();
    let mut used = BTreeSet::new();
    for doc in plan.documents.iter().filter(|d| d.include) {
        let parent = doc.path.rsplit_once('/').map_or("", |(p, _)| p);
        for summary in space.find_by_property(crate::property_keys::PATH, &doc.path)? {
            let Some(node) = space.get_node(&summary.id)? else {
                continue;
            };
            let mut body = node.body.clone();
            let references: Vec<_> = Parser::new_ext(&node.body, knode_md::options())
                .filter_map(|event| match event {
                    Event::Start(
                        Tag::Link {
                            link_type,
                            dest_url,
                            ..
                        }
                        | Tag::Image {
                            link_type,
                            dest_url,
                            ..
                        },
                    ) => Some((
                        dest_url.to_string(),
                        matches!(link_type, LinkType::WikiLink { .. }),
                    )),
                    _ => None,
                })
                .collect();
            for (written, wiki) in references {
                let decoded = percent_encoding::percent_decode_str(&written).decode_utf8_lossy();
                let target = decoded.split('#').next().unwrap_or(&decoded);
                if target.is_empty() || target.to_lowercase().ends_with(".md") {
                    continue;
                }
                if wiki && !space.find_by_title(target)?.is_empty() {
                    continue;
                }
                let adjacent = relative_asset(parent, target);
                let root = relative_asset("", target);
                let source = adjacent
                    .as_deref()
                    .filter(|p| assets.contains(p))
                    .or_else(|| root.as_deref().filter(|p| assets.contains(p)))
                    .map(str::to_owned)
                    .or_else(|| {
                        if target.contains('/') {
                            return None;
                        }
                        let matches: Vec<_> = assets
                            .iter()
                            .filter(|p| p.rsplit('/').next() == Some(target))
                            .collect();
                        (matches.len() == 1).then(|| matches[0].to_string())
                    });
                let Some(source) = source else {
                    continue;
                };
                let attachment = space.attach_file(&node.id, input.join(&source), by)?;
                if wiki {
                    body = knode_md::rename_links(&body, target, &attachment.name);
                } else {
                    body = crate::attachments::files_to_blob_urls(
                        &body,
                        &BTreeMap::from([(written, attachment.hash.clone())]),
                    );
                }
                used.insert(source.clone());
                adapted.push(AttachmentLink {
                    document: doc.path.clone(),
                    source,
                    name: attachment.name,
                });
            }
            if body != node.body {
                space.update_node(
                    &node.id,
                    knode_format::NodeUpdate {
                        body: Some(body),
                        ..Default::default()
                    },
                    by,
                )?;
            }
        }
    }
    report.attachments.retain(|path| !used.contains(path));
    for path in used {
        if !report.attached.contains(&path) {
            report.attached.push(path);
        }
    }
    report.attached.sort();
    Ok(adapted)
}

/// Archive, never delete, an onboarding-created package. Close all service/editor handles first.
/// Reopen the returned .knode path to recover it; sources are never touched.
pub fn archive(path: &Path) -> Result<PathBuf> {
    let path = path.canonicalize().map_err(io_error(path))?;
    let receipt: Receipt =
        serde_json::from_slice(&fs::read(path.join(RECEIPT)).map_err(io_error(&path))?)
            .map_err(|e| invalid(e.to_string()))?;
    let manifest = Manifest::read(&path)?;
    if receipt.space_id != manifest.id {
        return Err(invalid("Import receipt does not match this space."));
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| invalid(e.to_string()))?
        .as_nanos();
    let target = path.with_file_name(format!(
        "{}-archive-{stamp}.knode",
        path.file_stem().unwrap().to_string_lossy()
    ));
    if target.exists() {
        return Err(invalid("Archive destination already exists."));
    }
    fs::rename(&path, &target).map_err(io_error(&path))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_merge_exclude_import_and_archive_preserve_originals() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        fs::create_dir_all(root.join("A")).unwrap();
        fs::create_dir_all(root.join("B")).unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::write(root.join("A/Plan.md"), "See [[Other]].").unwrap();
        fs::write(root.join("B/Other.md"), "original").unwrap();
        fs::write(root.join("Skip.md"), "excluded").unwrap();
        fs::write(root.join("node_modules/pkg/README.md"), "dependency").unwrap();
        let mut plan = analyze(Some(&root), Mode::Vault).unwrap();
        assert_eq!(plan.documents.len(), 3);
        for group in &mut plan.groups {
            group.name = "Merged project".into();
        }
        plan.documents
            .iter_mut()
            .find(|d| d.path == "Skip.md")
            .unwrap()
            .include = false;
        let dest = dir.path().join("Work.knode");
        let receipt = apply(&plan, &dest, "Work", &Provenance::person("Tester")).unwrap();
        assert_eq!(receipt.contexts.len(), 1);
        let space = Space::open(&dest).unwrap();
        assert_eq!(space.node_count().unwrap(), 3);
        assert_eq!(
            space.setting("capture.enabled").unwrap().as_deref(),
            Some("false")
        );
        assert_eq!(
            space
                .links_from(&space.find_by_title("Plan").unwrap()[0].id)
                .unwrap()
                .len(),
            2
        );
        let from = space.find_by_title("Plan").unwrap()[0].id;
        assert!(matches!(
            space.resolve_link(&from, "B/Other#Section").unwrap(),
            Some(knode_format::LinkTarget::Node(_))
        ));
        drop(space);
        assert_eq!(
            fs::read_to_string(root.join("B/Other.md")).unwrap(),
            "original"
        );
        assert!(apply(&plan, &dest, "Work", &Provenance::system()).is_err());
        let archived = archive(&dest).unwrap();
        assert!(!dest.exists());
        assert!(archived.exists());
        assert_eq!(Space::open(archived).unwrap().node_count().unwrap(), 3);
    }
    #[test]
    fn rejects_stale_plans_and_source_destinations_without_partial_spaces() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("A.md"), "old").unwrap();
        let plan = analyze(Some(dir.path()), Mode::Project).unwrap();
        let dest = dir.path().join("Nested.knode");
        assert!(apply(&plan, &dest, "Nested", &Provenance::system()).is_err());
        assert!(!dest.exists());
        fs::write(dir.path().join("A.md"), "changed").unwrap();
        let parent = tempfile::tempdir().unwrap();
        let dest = parent.path().join("Work.knode");
        assert!(apply(&plan, &dest, "Work", &Provenance::system()).is_err());
        assert!(!dest.exists());
    }
    #[test]
    fn shared_relative_and_colliding_assets_are_kept_without_changing_source() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("vault");
        for folder in ["Notes", "assets/one", "assets/two"] {
            fs::create_dir_all(source.join(folder)).unwrap();
        }
        fs::write(
            source.join("assets/one/photo.png"),
            b"first synthetic image",
        )
        .unwrap();
        fs::write(
            source.join("assets/two/photo.png"),
            b"second synthetic image",
        )
        .unwrap();
        fs::write(source.join("assets/unique.pdf"), b"synthetic pdf").unwrap();
        let original = "![[assets/one/photo.png|caption]] ![[assets/two/photo.png]] ![reading](../assets/unique.pdf) `![[assets/one/photo.png]]`";
        fs::write(source.join("Notes/Plan.md"), original).unwrap();
        let plan = analyze(Some(&source), Mode::Vault).unwrap();
        let destination = dir.path().join("Assets.knode");
        let receipt = apply(&plan, &destination, "Assets", &Provenance::person("Tester")).unwrap();
        assert_eq!(receipt.import.attached.len(), 3);
        let space = Space::open(&destination).unwrap();
        let id = space.find_by_title("Plan").unwrap()[0].id;
        assert_eq!(space.attachments(&id).unwrap().len(), 3);
        let body = space.get_node(&id).unwrap().unwrap().body;
        assert!(body.contains("![[photo.png|caption]]"));
        assert!(body.contains("![[photo 2.png]]"));
        assert!(body.contains("knode:blob/"));
        assert!(body.contains("`![[assets/one/photo.png]]`"));
        assert_eq!(
            fs::read_to_string(source.join("Notes/Plan.md")).unwrap(),
            original
        );
    }

    #[test]
    fn project_without_markdown_starts_with_one_editable_context() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("repo");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("main.rs"), "fn main() {}").unwrap();
        let plan = analyze(Some(&source), Mode::Project).unwrap();
        assert_eq!(plan.groups.len(), 1);
        let receipt = apply(
            &plan,
            &dir.path().join("Project.knode"),
            "Project",
            &Provenance::person("Tester"),
        )
        .unwrap();
        assert_eq!(receipt.contexts.len(), 1);
    }
}
