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
/// The kind of the node made for each project, `kinds::PROJECT` once that lands in the format.
const PROJECT_KIND: &str = "project";
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
/// File extensions that are usually attachments of a note, for warnings about missing ones.
const ASSETS: &[&str] = &[
    "png",
    "jpg",
    "jpeg",
    "gif",
    "webp",
    "svg",
    "bmp",
    "tif",
    "tiff",
    "heic",
    "avif",
    "pdf",
    "mp3",
    "m4a",
    "wav",
    "ogg",
    "flac",
    "mp4",
    "mov",
    "webm",
    "mkv",
    "zip",
    "csv",
    "docx",
    "xlsx",
    "pptx",
    "excalidraw",
];
/// Folders named like this hold one project per subfolder.
const PROJECT_FOLDERS: &[&str] = &["Projects", "projects", "Proyectos", "proyectos"];
const HIDDEN: &str = "hidden files and folders";
const GENERATED: &str = "generated or dependency folders";
const SYMLINKS: &str = "symbolic links";
const SPACES: &str = "nested Knode spaces";
const WORKTREES: &str =
    "git worktrees (another checkout of a repository already in the source, or kept elsewhere)";
const SUBMODULES: &str = "git submodules (the content of another repository)";
const LINKED: &str = "linked git checkouts";
const OVERSIZED: &str = "oversized files";
const CREDENTIALS: &str = "possible credential files";
const NON_UTF8: &str = "files with non-UTF-8 names";

/// What a walk of the source found, before any file is read.
#[derive(Default)]
struct Inventory {
    /// Eligible files, relative paths.
    files: Vec<String>,
    /// Folders below the source that are git repositories of their own.
    repos: BTreeSet<String>,
    /// Excluded paths, by reason.
    excluded: BTreeMap<&'static str, Vec<String>>,
    /// Per-file notes, such as canvases.
    warnings: Vec<String>,
}
impl Inventory {
    fn exclude(&mut self, reason: &'static str, path: String) {
        self.excluded.entry(reason).or_default().push(path);
    }
}
/// How a folder relates to git.
enum Git {
    /// Not the top of a checkout.
    None,
    /// The top of a repository with its own `.git` folder.
    Repo,
    /// A checkout whose `.git` is a file pointing elsewhere: a worktree or a submodule.
    Linked(&'static str),
}
fn git(dir: &Path) -> Git {
    let marker = dir.join(".git");
    match fs::symlink_metadata(&marker) {
        Ok(meta) if meta.is_dir() => Git::Repo,
        Ok(meta) if meta.is_file() => {
            let pointer = fs::read_to_string(&marker).unwrap_or_default();
            Git::Linked(if pointer.contains("/worktrees/") {
                WORKTREES
            } else if pointer.contains("/modules/") {
                SUBMODULES
            } else {
                LINKED
            })
        }
        _ => Git::None,
    }
}
fn inventory(root: &Path, prefix: &str, found: &mut Inventory) -> Result<()> {
    for entry in fs::read_dir(root).map_err(io_error(root))? {
        let entry = entry.map_err(io_error(root))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            found.exclude(NON_UTF8, entry.file_name().to_string_lossy().into_owned());
            continue;
        };
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let meta = fs::symlink_metadata(entry.path()).map_err(io_error(entry.path()))?;
        if name.starts_with('.') {
            found.exclude(HIDDEN, rel);
            continue;
        }
        if meta.file_type().is_symlink() {
            found.exclude(SYMLINKS, rel);
            continue;
        }
        if meta.is_dir() {
            if OMIT.contains(&name.as_str()) {
                found.exclude(GENERATED, rel);
                continue;
            }
            if name.ends_with(".knode") {
                found.exclude(SPACES, rel);
                continue;
            }
            match git(&entry.path()) {
                Git::Linked(reason) => {
                    found.exclude(reason, rel);
                    continue;
                }
                Git::Repo => {
                    found.repos.insert(rel.clone());
                }
                Git::None => {}
            }
            inventory(&entry.path(), &rel, found)?;
        } else if meta.is_file() {
            if meta.len() > knode_format::MAX_ATTACHMENT_SIZE {
                found.exclude(OVERSIZED, rel);
                continue;
            }
            if ["pem", "key", "p12", "pfx"]
                .iter()
                .any(|ext| name.ends_with(&format!(".{ext}")))
            {
                found.exclude(CREDENTIALS, rel);
                continue;
            }
            if name.ends_with(".canvas") {
                found.warnings.push(format!(
                    "{rel}: Obsidian canvas layout is not converted; review it separately."
                ));
            }
            found.files.push(rel);
        }
    }
    Ok(())
}
/// One line per exclusion reason, with a few examples, instead of one line per path.
fn summarize(excluded: &BTreeMap<&'static str, Vec<String>>) -> Vec<String> {
    excluded
        .iter()
        .map(|(reason, paths)| {
            let shown: Vec<_> = paths.iter().take(3).map(String::as_str).collect();
            let more = paths.len().saturating_sub(shown.len());
            let more = if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            };
            format!(
                "Excluded {} {reason}: {}{more}.",
                paths.len(),
                shown.join(", ")
            )
        })
        .collect()
}
/// Top-level folders that hold repositories in two or more of their subfolders, such as a folder
/// per organization, with those subfolders: each becomes a project of its own, inside the
/// top-level one. Other subfolders, such as notes about the whole, stay with the top-level folder.
fn umbrellas(repos: &BTreeSet<String>) -> BTreeMap<String, BTreeSet<String>> {
    let mut children: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for repo in repos {
        let mut parts = repo.split('/');
        if let (Some(top), Some(child)) = (parts.next(), parts.next()) {
            children
                .entry(top.to_owned())
                .or_default()
                .insert(child.to_owned());
        }
    }
    children.retain(|top, inside| inside.len() >= 2 && !repos.contains(top));
    children
}
/// The suggested group of a document: its top-level folder, or a project subfolder of a folder of
/// projects. Files at the top of the source are shared.
fn group_for(path: &str, mode: Mode, umbrellas: &BTreeMap<String, BTreeSet<String>>) -> String {
    if mode == Mode::Project {
        return "project".into();
    }
    let parts: Vec<_> = path.split('/').collect();
    match parts.as_slice() {
        [] | [_] => "shared".into(),
        [top, child, _, ..]
            if PROJECT_FOLDERS.contains(top)
                || umbrellas.get(*top).is_some_and(|c| c.contains(*child)) =>
        {
            format!("{top}/{child}")
        }
        [top, ..] => (*top).to_owned(),
    }
}
/// The destinations a note links to, decoded and without `#heading`, skipping web addresses.
fn references(body: &str) -> Vec<(String, bool)> {
    use pulldown_cmark::{Event, LinkType, Parser, Tag};
    Parser::new_ext(body, knode_md::options())
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
        .filter(|(dest, _)| !dest.contains("://") && !dest.starts_with("mailto:"))
        .collect()
}
fn target_of(written: &str) -> String {
    let decoded = percent_encoding::percent_decode_str(written).decode_utf8_lossy();
    decoded.split('#').next().unwrap_or(&decoded).to_owned()
}
fn is_asset(target: &str) -> bool {
    Path::new(target)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ASSETS.contains(&e.to_lowercase().as_str()))
}
/// The source file an attachment reference names: next to the note, from the source root, or
/// the only file anywhere with that name. Ambiguous names are not guessed.
fn resolve_asset(parent: &str, target: &str, assets: &BTreeSet<&str>) -> Option<String> {
    relative_asset(parent, target)
        .filter(|p| assets.contains(p.as_str()))
        .or_else(|| relative_asset("", target).filter(|p| assets.contains(p.as_str())))
        .or_else(|| {
            if target.contains('/') {
                return None;
            }
            let matches: Vec<_> = assets
                .iter()
                .filter(|p| p.rsplit('/').next() == Some(target))
                .collect();
            (matches.len() == 1).then(|| matches[0].to_string())
        })
}
/// Analyze locally. Project guesses follow folders and repositories, not inferred meanings or
/// decisions. Only Markdown files and the files they reference are read.
pub fn analyze(source: Option<&Path>, mode: Mode) -> Result<Plan> {
    let source = match (source, mode) {
        (None, Mode::Empty) => None,
        (Some(path), m) if m != Mode::Empty => Some(path.canonicalize().map_err(io_error(path))?),
        _ => return Err(invalid("Choose a source folder, or choose an empty start.")),
    };
    let mut plan = Plan {
        version: 1,
        mode,
        source,
        groups: vec![],
        documents: vec![],
        files: vec![],
        warnings: vec![],
    };
    let mut notices = vec!["Folder-based suggestions only: no AI summaries or inferred decisions. Review assignments. Sources stay unchanged; changes in Knode are not synchronized back. Plugin behavior is not migrated. Review documents for private information before connecting an external agent.".to_owned()];
    if let Some(root) = &plan.source {
        let mut found = Inventory::default();
        inventory(root, "", &mut found)?;
        found.files.sort();
        let umbrellas = umbrellas(&found.repos);
        let (notes, others): (Vec<_>, Vec<_>) =
            found.files.iter().partition(|p| p.ends_with(".md"));
        let others: BTreeSet<&str> = others.into_iter().map(String::as_str).collect();
        let mut groups = BTreeMap::new();
        let mut referenced = BTreeSet::new();
        let mut missing = Vec::new();
        for path in notes {
            let full = root.join(path);
            let digest = digest(&full)?;
            plan.files.push(SourceFile {
                path: path.clone(),
                digest: digest.clone(),
            });
            let text = match fs::read_to_string(&full) {
                Ok(t) => t,
                Err(e) => {
                    found
                        .warnings
                        .push(format!("{path}: not imported as UTF-8 Markdown ({e})."));
                    continue;
                }
            };
            let note = crate::import::parse(path, &text);
            let id = group_for(path, mode, &umbrellas);
            let name = match id.as_str() {
                "project" => root
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                "shared" => "Shared context".into(),
                _ => id.rsplit('/').next().unwrap_or(&id).to_owned(),
            };
            groups.insert(id.clone(), name);
            if text.contains("```dataview") || text.contains("```tasks") || text.contains("<%") {
                found.warnings.push(format!(
                    "{path}: possible plugin/template code is preserved as text, not executed."
                ));
            }
            let parent = path.rsplit_once('/').map_or("", |(p, _)| p);
            for (written, _) in references(&note.body) {
                let target = target_of(&written);
                if target.is_empty() || target.to_lowercase().ends_with(".md") {
                    continue;
                }
                if !is_asset(&target) {
                    continue;
                }
                referenced.insert(target.rsplit('/').next().unwrap_or(&target).to_owned());
                if resolve_asset(parent, &target, &others).is_none() {
                    missing.push(format!("{path} → {target}"));
                }
            }
            plan.documents.push(Document {
                path: path.clone(),
                title: note.title,
                project: id,
                include: true,
                digest,
            });
        }
        // Only files a note may reference are copied and fingerprinted: every file with a
        // referenced name, so a name that is unique in the source stays unique here.
        for path in others {
            if referenced.contains(path.rsplit('/').next().unwrap_or(path)) {
                plan.files.push(SourceFile {
                    path: path.to_owned(),
                    digest: digest(&root.join(path))?,
                });
            }
        }
        plan.files.sort_by(|a, b| a.path.cmp(&b.path));
        // A folder of projects is a group too, so its projects have somewhere to belong.
        let nested: BTreeSet<_> = groups
            .keys()
            .filter_map(|id| id.split_once('/').map(|(top, _)| top.to_owned()))
            .filter(|top| umbrellas.contains_key(top))
            .collect();
        for top in nested {
            groups.entry(top.clone()).or_insert(top);
        }
        if !missing.is_empty() {
            let shown: Vec<_> = missing.iter().take(5).map(String::as_str).collect();
            let more = missing.len() - shown.len();
            notices.push(format!(
                "{} attachment references were not found in the source, or match several files, and stay as written: {}{}.",
                missing.len(),
                shown.join("; "),
                if more > 0 { format!(" and {more} more") } else { String::new() }
            ));
        }
        notices.extend(summarize(&found.excluded));
        plan.groups = unique_names(groups);
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
        found.warnings.sort();
        found.warnings.dedup();
        notices.extend(found.warnings);
    } else {
        plan.groups.push(Group {
            id: "project".into(),
            name: "My project".into(),
        });
    }
    plan.warnings = notices;
    Ok(plan)
}
/// Groups with the same suggested name would merge on import, since equal names merge on
/// purpose: tell them apart by their parent folder instead.
fn unique_names(groups: BTreeMap<String, String>) -> Vec<Group> {
    let mut count: BTreeMap<&str, usize> = BTreeMap::new();
    for name in groups.values() {
        *count.entry(name).or_default() += 1;
    }
    groups
        .iter()
        .map(|(id, name)| {
            let name = match id.rsplit_once('/') {
                Some((parent, _)) if count[name.as_str()] > 1 => format!("{name} ({parent})"),
                _ => name.clone(),
            };
            Group {
                id: id.clone(),
                name,
            }
        })
        .collect()
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
            // The importer keeps the dates of the files it reads: give the copy the source's.
            if let Ok(modified) = fs::metadata(&source_file).and_then(|m| m.modified()) {
                let _ = fs::File::options()
                    .write(true)
                    .open(&out)
                    .and_then(|f| f.set_modified(modified));
            }
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
    // A group inside another (a project in a folder of projects) belongs to its parent's index.
    let parents: BTreeMap<String, String> = groups
        .iter()
        .filter_map(|(id, name)| {
            let (parent, _) = id.rsplit_once('/')?;
            let parent = groups.get(parent)?;
            (parent != name).then(|| (name.clone(), parent.clone()))
        })
        .collect();
    let mut owner: BTreeMap<knode_format::NodeId, String> = BTreeMap::new();
    let mut members: BTreeMap<String, Vec<(&Document, knode_format::NodeId)>> = BTreeMap::new();
    for doc in plan.documents.iter().filter(|d| d.include) {
        let project = &groups[&doc.project];
        for node in space.find_by_property(crate::property_keys::PATH, &doc.path)? {
            owner.insert(node.id, project.clone());
            members
                .entry(project.clone())
                .or_default()
                .push((doc, node.id));
        }
    }
    // Links between notes of different projects say how the projects relate.
    let mut related: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (from, project) in &owner {
        for link in space.links_from(from)? {
            if let Some(other) = owner.get(&link.to)
                && other != project
            {
                let pair = if project < other {
                    (project.clone(), other.clone())
                } else {
                    (other.clone(), project.clone())
                };
                *related.entry(pair).or_default() += 1;
            }
        }
    }
    // A group whose notes link to a large share of the others, such as a folder of maps or
    // dashboards, is an index of everything rather than a related project: leave its pairs out.
    let total = groups.values().collect::<BTreeSet<_>>().len();
    let mut reach: BTreeMap<&str, usize> = BTreeMap::new();
    for (a, b) in related.keys() {
        *reach.entry(a).or_default() += 1;
        *reach.entry(b).or_default() += 1;
    }
    let hubs: BTreeSet<String> = reach
        .into_iter()
        .filter(|(_, n)| *n >= 5 && *n * 5 >= total)
        .map(|(name, _)| name.to_owned())
        .collect();
    related.retain(|(a, b), _| !hubs.contains(a) && !hubs.contains(b));
    let names: BTreeSet<_> = groups.values().cloned().collect();
    for project in &names {
        let has_children = parents.values().any(|p| p == project);
        if !members.contains_key(project) && !has_children && plan.mode == Mode::Vault {
            continue;
        }
        // A node of kind `project` (format kind `project`): the app and MCP list it as one, so
        // it keeps the project's own name even when a note has the same title.
        let context = space.create_node(
            NewNode {
                kind: PROJECT_KIND.into(),
                title: project.clone(),
                ..Default::default()
            },
            by,
        )?;
        space.set_property(&context.id, "onboarding.generated", "project-context", by)?;
        contexts.insert(project.clone(), context.id);
    }
    for (project, id) in &contexts {
        let mine = members.get(project).map(Vec::as_slice).unwrap_or_default();
        let folders: BTreeSet<_> = mine.iter().map(|(doc, _)| doc.project.as_str()).collect();
        let mut body = format!("# {project}\n\n");
        if mine.is_empty() {
            body.push_str("A folder of projects. ");
        } else {
            body.push_str(&format!(
                "Index of the {} notes imported from {}. ",
                mine.len(),
                folders
                    .iter()
                    .map(|f| if *f == "shared" || *f == "project" {
                        "the top of the source".to_owned()
                    } else {
                        format!("`{}/`", f.replace('`', " "))
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        body.push_str("Onboarding made this project from the folder structure; edit this text freely. Each imported note is part of it.\n");
        if !mine.is_empty() {
            let mut key: Vec<_> = mine.iter().collect();
            key.sort_by_key(|(doc, _)| {
                (
                    doc.path.matches('/').count(),
                    importance(&doc.path),
                    &doc.path,
                )
            });
            body.push_str("\n## Start here\n\n");
            for (doc, node) in key.iter().take(8) {
                body.push_str(&format!(
                    "- [[{node}|{}]] — `{}`\n",
                    doc.title.replace([']', '|'], " "),
                    doc.path.replace('`', " ")
                ));
            }
        }
        let children: Vec<_> = parents
            .iter()
            .filter(|(_, parent)| *parent == project)
            .filter_map(|(child, _)| contexts.get(child).map(|c| (child, c)))
            .collect();
        if !children.is_empty() {
            body.push_str("\n## Projects inside\n\n");
            for (child, child_id) in &children {
                body.push_str(&format!(
                    "- [[{child_id}|{}]]\n",
                    child.replace([']', '|'], " ")
                ));
            }
        }
        let mut near: Vec<_> = related
            .iter()
            .filter_map(|((a, b), n)| {
                let other = if a == project {
                    b
                } else if b == project {
                    a
                } else {
                    return None;
                };
                Some((*n, other, contexts.get(other)?))
            })
            .collect();
        near.sort_by(|x, y| y.0.cmp(&x.0).then(x.1.cmp(y.1)));
        if !near.is_empty() {
            body.push_str("\n## Related\n\n");
            for (n, other, other_id) in near.iter().take(12) {
                let s = if *n == 1 { "" } else { "s" };
                body.push_str(&format!(
                    "- [[{other_id}|{}]] — {n} link{s} between their notes\n",
                    other.replace([']', '|'], " ")
                ));
            }
        }
        space.update_node(
            id,
            knode_format::NodeUpdate {
                body: Some(body),
                ..Default::default()
            },
            by,
        )?;
        for (_, node) in mine {
            space.link(node, id, "part-of", by)?;
        }
        if let Some(parent) = parents.get(project).and_then(|p| contexts.get(p)) {
            space.link(id, parent, "part-of", by)?;
        }
    }
    for (a, b) in related.keys() {
        if let (Some(a), Some(b)) = (contexts.get(a), contexts.get(b)) {
            space.link(a, b, knode_format::link_kinds::RELATED, by)?;
        }
    }
    let contexts: BTreeMap<String, String> = contexts
        .into_iter()
        .map(|(name, id)| (name, id.to_string()))
        .collect();
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
/// Notes to list first in a project's index: its readme, status and agent instructions.
fn importance(path: &str) -> usize {
    let stem = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".md")
        .to_lowercase();
    [
        "readme", "estado", "status", "index", "overview", "claude", "agents", "roadmap",
    ]
    .iter()
    .position(|k| *k == stem)
    .unwrap_or(usize::MAX)
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
            for (written, wiki) in references(&node.body) {
                let target = target_of(&written);
                let target = target.as_str();
                // Attachments only: links to code or other text files stay as written.
                if !is_asset(target) {
                    continue;
                }
                if wiki && !space.find_by_title(target)?.is_empty() {
                    continue;
                }
                let source = resolve_asset(parent, target, &assets);
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

/// The name of a space without the `-archive-<stamp>` its archive added, so a recovered space
/// archived again is `Work-archive-<new stamp>`, not `Work-archive-<old>-archive-<new>`.
fn unarchived_stem(stem: &str) -> &str {
    match stem.rsplit_once("-archive-") {
        Some((base, stamp)) if !stamp.is_empty() && stamp.bytes().all(|b| b.is_ascii_digit()) => {
            unarchived_stem(base)
        }
        _ => stem,
    }
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
        unarchived_stem(&path.file_stem().unwrap().to_string_lossy())
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
    use knode_format::NodeId;
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
        assert_eq!(Space::open(&archived).unwrap().node_count().unwrap(), 3);
        // Archived again after recovery, the name keeps one archive stamp.
        let again = archive(&archived).unwrap();
        let name = again.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("Work-archive-") && name.matches("-archive-").count() == 1,
            "{name}"
        );
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
    fn repositories_split_folders_of_projects_and_skip_other_checkouts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        // A folder of two repositories, with notes about the whole and two linked checkouts.
        write("Org/notes/Plan.md", "The plan for the organization.");
        write("Org/alpha/README.md", "Alpha builds on [[Beta notes]].");
        write("Org/beta/Beta notes.md", "Beta.");
        fs::create_dir_all(root.join("Org/alpha/.git")).unwrap();
        fs::create_dir_all(root.join("Org/beta/.git")).unwrap();
        write(
            "Org/beta-wt/.git",
            "gitdir: /elsewhere/.git/worktrees/beta-wt\n",
        );
        write("Org/beta-wt/Beta notes.md", "Beta.");
        write("Org/alpha/brand/.git", "gitdir: ../.git/modules/brand\n");
        write("Org/alpha/brand/README.md", "Shared brand.");
        // An attachment, a link to code and a file nothing references.
        write(
            "Solo/README.md",
            "![shot](shot.png) and [the code](main.rs).",
        );
        write("Solo/shot.png", "synthetic image");
        write("Solo/main.rs", "fn main() {}");
        write("Solo/unused.png", "unused image");
        // A folder of maps linking every project is an index, not a related project.
        let mut maps = String::new();
        for i in 1..=5 {
            write(&format!("P{i}/Note {i}.md"), "A project.");
            maps.push_str(&format!("[[Note {i}]] "));
        }
        write("Maps/All.md", &maps);
        let plan = analyze(Some(&root), Mode::Vault).unwrap();
        let ids: BTreeSet<_> = plan.groups.iter().map(|g| g.id.as_str()).collect();
        for id in ["Org", "Org/alpha", "Org/beta", "Solo", "Maps"] {
            assert!(ids.contains(id), "{id} in {ids:?}");
        }
        assert!(!ids.contains("Org/notes"));
        assert!(
            plan.documents
                .iter()
                .all(|d| !d.path.contains("beta-wt") && !d.path.contains("brand"))
        );
        let files: BTreeSet<_> = plan.files.iter().map(|f| f.path.as_str()).collect();
        assert!(files.contains("Solo/shot.png"));
        assert!(!files.contains("Solo/unused.png") && !files.contains("Solo/main.rs"));
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.starts_with("Excluded 1 git worktrees"))
        );
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.starts_with("Excluded 1 git submodules"))
        );

        let dest = dir.path().join("Org.knode");
        let receipt = apply(&plan, &dest, "Org", &Provenance::person("Tester")).unwrap();
        let space = Space::open(&dest).unwrap();
        let context = |name: &str| NodeId::parse(&receipt.contexts[name]).unwrap();
        let parts = space.links_from(&context("alpha")).unwrap();
        assert!(
            parts
                .iter()
                .any(|l| l.kind == "part-of" && l.to == context("Org"))
        );
        let related: Vec<_> = receipt
            .contexts
            .values()
            .flat_map(|id| space.links_from(&NodeId::parse(id).unwrap()).unwrap())
            .filter(|l| l.kind == knode_format::link_kinds::RELATED)
            .collect();
        assert_eq!(related.len(), 1);
        let pair = BTreeSet::from([related[0].from, related[0].to]);
        assert_eq!(pair, BTreeSet::from([context("alpha"), context("beta")]));
        let readme = space
            .find_by_property(crate::property_keys::PATH, "Solo/README.md")
            .unwrap();
        assert_eq!(space.attachments(&readme[0].id).unwrap().len(), 1);
        assert_eq!(
            space.get_node(&context("Org")).unwrap().unwrap().kind,
            "project"
        );
        let body = space.get_node(&context("Org")).unwrap().unwrap().body;
        assert!(body.contains("## Projects inside") && !body.contains("To be filled"));
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
