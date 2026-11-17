//! The name, settings and folders of a space: the shapes of `space.rename`, `settings.get`,
//! `settings.set`, `folders.list`, `folders.set`, `folders.remove` and `folders.overlaps`, and the
//! rules they follow (section 15 of the .knode specification).
//!
//! These functions work on an open space, so the CLI can use them when the service cannot run.
//! Only a person changes the name, the settings and the folders of a space.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use knode_format::{
    ActorKind, Folder, FolderProject, FolderSpec, Provenance, Space, folder_kinds, setting_keys,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::markdown::{record_path, resolve_path};
use crate::protocol::SpaceInfo;

/// The most characters (Unicode scalar values) an icon may have: one emoji, even one made of
/// several joined together, such as a family.
const ICON_MAX_CHARS: usize = 16;

/// A folder of a space, as `folders.list` and `folders.set` return it: the folder as the space
/// records it, and where it is on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderInfo {
    /// The folder as the space records it.
    #[serde(flatten)]
    pub folder: Folder,
    /// The folder on this machine, or `None` for a path that cannot be resolved here.
    pub local: Option<PathBuf>,
    /// Whether the folder is missing on this machine. A missing folder is kept and skipped.
    pub missing: bool,
    /// For a project folder, the `project` node of the same name it holds the files of
    /// (section 15.2): `created` when adding the folder made it. `None` for other folders, and
    /// for a project folder added before Knode made these, until it is added again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<FolderProject>,
}

impl FolderInfo {
    fn of(folder: Folder, project: Option<FolderProject>) -> Self {
        let local = resolve_path(&folder.path);
        let missing = !local.as_deref().is_some_and(Path::exists);
        FolderInfo {
            folder,
            local,
            missing,
            project,
        }
    }

    /// A folder as the space has it, with the project it holds the files of, if any.
    fn read(space: &Space, folder: Folder) -> knode_format::Result<Self> {
        let project = space.folder_project(&folder)?.map(|node| FolderProject {
            node,
            created: false,
        });
        Ok(FolderInfo::of(folder, project))
    }
}

/// A folder to add or change with `folders.set`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderInput {
    /// The id of the folder to change, or `None` to add one (or change the folder with the
    /// same kind and path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// `project`, `notes` or `agent`.
    pub kind: String,
    /// The folder: an absolute path, or one starting with `~/`. It is recorded as the
    /// specification says (section 15.3), relative to the home folder when it is inside it.
    pub path: String,
    /// Whether the folder is captured and read. On when left out.
    #[serde(default = "on")]
    pub capture: bool,
    /// The rules of the folder (section 15.4): `exclude` for projects and notes, `include` for
    /// notes, `agent` (required) for agent folders.
    #[serde(default = "empty_object")]
    pub rules: Value,
}

fn on() -> bool {
    true
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

/// What changed in a space, other than its nodes: the params of a `space` notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceEvent {
    /// The space, as the service opened it.
    pub space: PathBuf,
    /// What changed.
    pub what: SpaceEventKind,
    /// The new name, for `renamed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The setting that changed, for `settings`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The id of the folder that was added, changed or removed, for `folders`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// The kind of link that was defined, changed or removed, for `link_kinds`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_kind: Option<String>,
    /// The id of the view that was created, changed or removed, for `views`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
}

/// What changed in a [`SpaceEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpaceEventKind {
    /// The space was renamed.
    Renamed,
    /// A setting changed, such as its icon.
    Settings,
    /// A folder was added, changed or removed.
    Folders,
    /// A kind of link was defined, changed or removed.
    #[serde(rename = "link_kinds")]
    LinkKinds,
    /// A graph view or a canvas was created, changed or removed, or nodes or groups were
    /// placed on it.
    Views,
}

fn person(by: &Provenance, what: &str) -> knode_format::Result<()> {
    if by.kind == ActorKind::Person {
        Ok(())
    } else {
        Err(knode_format::Error::PersonOnly(what.into()))
    }
}

fn invalid(message: impl Into<String>) -> knode_format::Error {
    knode_format::Error::Invalid(message.into())
}

/// Renames the space: changes its name in `manifest.json`. Only a person can rename a space.
pub fn rename(space: &mut Space, name: &str, by: &Provenance) -> knode_format::Result<SpaceInfo> {
    person(by, "rename a space")?;
    let name = name.trim();
    if name.chars().any(char::is_control) {
        return Err(invalid(
            "the name of a space cannot have control characters",
        ));
    }
    if name != space.manifest().name {
        space.rename(name)?;
    }
    SpaceInfo::of(space)
}

/// Every setting of the space, by key, `space.id` included. Absent keys have their default
/// (section 15.1).
pub fn settings(space: &Space) -> knode_format::Result<BTreeMap<String, String>> {
    space.settings()
}

/// Whether captured conversations are written to the space: its `capture.enabled` setting.
/// Anything other than `on` means off; absent means on.
pub fn capture_enabled(space: &Space) -> knode_format::Result<bool> {
    Ok(space
        .setting(setting_keys::CAPTURE_ENABLED)?
        .is_none_or(|value| value == "on"))
}

/// Changes a setting of the space, or puts it back to its default with `None`, and records the
/// change. Returns whether anything changed. Only a person can change settings.
///
/// The settings that can be changed this way are `space.icon` (one emoji) and
/// `capture.enabled` (`on` or `off`). `space.id` never changes, and the live Markdown folder
/// (`markdown.folder`) is changed with `markdown.folder.set`. A value equal to the default is
/// stored as no value, as the specification asks.
pub fn set_setting(
    space: &mut Space,
    key: &str,
    value: Option<&str>,
    by: &Provenance,
) -> knode_format::Result<bool> {
    person(by, "change the settings of a space")?;
    let value = value.map(str::trim);
    let value = match key {
        setting_keys::SPACE_ICON => match value {
            None | Some("") => None,
            Some(icon) => Some(check_icon(icon)?),
        },
        setting_keys::CAPTURE_ENABLED => match value {
            None | Some("on") => None,
            Some("off") => Some("off"),
            Some(other) => {
                return Err(invalid(format!(
                    "capture.enabled is \"on\" or \"off\", not {other:?}"
                )));
            }
        },
        setting_keys::SPACE_ID => return Err(invalid("the id of a space never changes")),
        setting_keys::MARKDOWN_FOLDER => {
            return Err(invalid(
                "the live Markdown folder is changed with markdown.folder.set \
                 (`knode markdown folder`)",
            ));
        }
        other => {
            return Err(invalid(format!(
                "{other:?} is not a setting Knode knows: use space.icon or capture.enabled"
            )));
        }
    };
    space.set_setting(key, value, by)
}

/// Checks an icon: one emoji. Plain text, such as letters or words, is refused.
fn check_icon(icon: &str) -> knode_format::Result<&str> {
    let chars = icon.chars().count();
    let ok = chars <= ICON_MAX_CHARS
        && !icon.chars().any(|c| c.is_whitespace() || c.is_control())
        && !icon.is_ascii()
        && !icon.chars().any(char::is_alphabetic);
    if ok {
        Ok(icon)
    } else {
        Err(invalid(format!(
            "the icon of a space is one emoji, such as 🌱, not {icon:?}"
        )))
    }
}

/// The folders that feed the space, oldest first, with where each one is on this machine.
pub fn folders(space: &Space) -> knode_format::Result<Vec<FolderInfo>> {
    space
        .folders()?
        .into_iter()
        .map(|folder| FolderInfo::read(space, folder))
        .collect()
}

/// Adds a folder to the space, or changes one, and records the change. Only a person can
/// change folders.
///
/// Adding a project folder (no `id`) also creates the `project` node named after it, or finds
/// the one the space has ([`Space::add_folder`]), so every client that adds folders gets the same
/// project; it is returned in [`FolderInfo::project`]. Changing a folder by its id creates
/// nothing.
pub fn set_folder(
    space: &mut Space,
    input: &FolderInput,
    by: &Provenance,
) -> knode_format::Result<FolderInfo> {
    person(by, "change the folders of a space")?;
    let kind = input.kind.trim();
    if ![
        folder_kinds::PROJECT,
        folder_kinds::NOTES,
        folder_kinds::AGENT,
    ]
    .contains(&kind)
    {
        return Err(invalid(format!(
            "a folder is a project, notes or agent folder, not {kind:?}"
        )));
    }
    let path = local_path(&input.path)?;
    if path.starts_with(space.path()) {
        return Err(invalid(
            "a folder of a space cannot be inside the space itself",
        ));
    }
    check_rules(kind, &input.rules)?;
    let spec = FolderSpec {
        id: input.id.clone(),
        kind: kind.to_string(),
        path: record_path(&path),
        capture: input.capture,
        rules: input.rules.clone(),
    };
    if spec.id.is_some() {
        let folder = space.set_folder(&spec, by)?;
        return FolderInfo::read(space, folder);
    }
    let added = space.add_folder(&spec, by)?;
    Ok(FolderInfo::of(added.folder, added.project))
}

/// Removes a folder from the space and records the change. Nothing captured or imported from
/// it is removed, and the `project` node of a project folder stays with everything part of it.
/// Returns whether there was one. Only a person can change folders.
pub fn remove_folder(space: &mut Space, id: &str, by: &Provenance) -> knode_format::Result<bool> {
    person(by, "change the folders of a space")?;
    space.remove_folder(id, by)
}

/// The folder on this machine for a path given as absolute or starting with `~`.
fn local_path(path: &str) -> knode_format::Result<PathBuf> {
    let path = path.trim();
    let local = if path == "~" || path.starts_with("~/") {
        resolve_path(path)
    } else {
        Some(PathBuf::from(path)).filter(|p| p.is_absolute())
    };
    local.ok_or_else(|| {
        invalid(format!(
            "a folder is an absolute path, or one that starts with ~/, not {path:?}"
        ))
    })
}

fn check_rules(kind: &str, rules: &Value) -> knode_format::Result<()> {
    let Some(rules) = rules.as_object() else {
        return Err(invalid("the rules of a folder are a JSON object"));
    };
    for key in ["include", "exclude"] {
        if let Some(patterns) = rules.get(key) {
            let ok = patterns
                .as_array()
                .is_some_and(|p| p.iter().all(|p| p.as_str().is_some_and(|p| !p.is_empty())));
            if !ok {
                return Err(invalid(format!(
                    "the rule {key} is a list of patterns, such as [\"node_modules/**\"]"
                )));
            }
        }
    }
    if kind == folder_kinds::AGENT {
        let agent = rules.get("agent").and_then(Value::as_str).unwrap_or("");
        if knode_ingest::Agent::from_id(agent).is_none() {
            let known: Vec<&str> = knode_ingest::Agent::ALL.iter().map(|a| a.id()).collect();
            return Err(invalid(format!(
                "an agent folder needs the rule agent, one of {}",
                known.join(", ")
            )));
        }
    }
    Ok(())
}

/// Whether the recorded path `path` is the folder `folder` or inside it.
pub(crate) fn inside(folder: &str, path: &str) -> bool {
    if folder == "/" {
        return path.starts_with('/');
    }
    path.strip_prefix(folder)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The folders of one space, for [`folder_overlaps`] and [`project_destinations`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceFolders {
    /// The space.
    pub space: PathBuf,
    /// Its name.
    pub name: String,
    /// Its folders, as the space records them.
    pub folders: Vec<Folder>,
}

impl SpaceFolders {
    /// The folders of an open space.
    pub fn of(space: &Space) -> knode_format::Result<Self> {
        Ok(SpaceFolders {
            space: space.path().to_path_buf(),
            name: space.manifest().name.clone(),
            folders: space.folders()?,
        })
    }
}

/// A project folder of a space, as [`folder_overlaps`] and [`project_destinations`] name it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MappedFolder {
    /// The space it belongs to.
    pub space: PathBuf,
    /// The name of that space: where the folder's conversations go.
    pub space_name: String,
    /// The id of the folder.
    pub folder: String,
    /// The folder as the space records it.
    pub path: String,
    /// Whether its conversations are captured.
    pub capture: bool,
}

impl MappedFolder {
    fn of(space: &SpaceFolders, folder: &Folder) -> Self {
        MappedFolder {
            space: space.space.clone(),
            space_name: space.name.clone(),
            folder: folder.id.clone(),
            path: folder.path.clone(),
            capture: folder.capture,
        }
    }
}

/// How two project folders overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverlapKind {
    /// The same folder is a project folder of two spaces: its conversations go to both.
    Same,
    /// One folder is inside the other: its conversations go to the inner folder's space only.
    Nested,
}

/// Two project folders that overlap, as `folders.overlaps` returns them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderOverlap {
    /// How they overlap.
    pub kind: OverlapKind,
    /// The outer folder; for [`OverlapKind::Same`], the folder of the first space.
    pub outer: MappedFolder,
    /// The folder inside it; for [`OverlapKind::Same`], the folder of the second space.
    pub inner: MappedFolder,
    /// What it means for capture, in a sentence, for people.
    pub message: String,
}

/// The project folders of `spaces` that overlap: the same folder in two spaces, or a folder
/// inside another. Capture sends a conversation to the project folder nearest to where it was
/// held, and to every space that has that folder, so both are worth a look: the same folder in
/// two spaces is captured into both, and a nested folder takes its conversations away from the
/// folder around it. Each pair once, in the order of `spaces` and their folders.
pub fn folder_overlaps(spaces: &[SpaceFolders]) -> Vec<FolderOverlap> {
    let projects: Vec<MappedFolder> = spaces
        .iter()
        .flat_map(|space| {
            space
                .folders
                .iter()
                .filter(|f| f.kind == folder_kinds::PROJECT)
                .map(move |f| MappedFolder::of(space, f))
        })
        .collect();
    let mut overlaps = Vec::new();
    for (i, a) in projects.iter().enumerate() {
        for b in &projects[i + 1..] {
            if a.path == b.path {
                if a.space == b.space {
                    continue; // A space keeps one folder per kind and path.
                }
                overlaps.push(FolderOverlap {
                    kind: OverlapKind::Same,
                    message: format!(
                        "{} is a project folder of both \"{}\" and \"{}\": its conversations are \
                         captured into both.",
                        a.path, a.space_name, b.space_name
                    ),
                    outer: a.clone(),
                    inner: b.clone(),
                });
                continue;
            }
            let (outer, inner) = if inside(&a.path, &b.path) {
                (a, b)
            } else if inside(&b.path, &a.path) {
                (b, a)
            } else {
                continue;
            };
            let message = if outer.space == inner.space {
                format!(
                    "{} is inside {}, both in \"{}\": conversations held in it belong to its own \
                     project, not to the one around it.",
                    inner.path, outer.path, inner.space_name
                )
            } else {
                format!(
                    "{} (\"{}\") is inside {} (\"{}\"): conversations held in it are captured \
                     into \"{}\" only, not into \"{}\".",
                    inner.path,
                    inner.space_name,
                    outer.path,
                    outer.space_name,
                    inner.space_name,
                    outer.space_name
                )
            };
            overlaps.push(FolderOverlap {
                kind: OverlapKind::Nested,
                outer: outer.clone(),
                inner: inner.clone(),
                message,
            });
        }
    }
    overlaps
}

/// The project folders a folder on this machine belongs to, as capture routes it: those
/// nearest to it, one per space (several when the same folder is in several spaces). Empty when
/// no project folder holds it. Whether capture is on is not considered.
pub fn project_destinations(spaces: &[SpaceFolders], folder: &Path) -> Vec<MappedFolder> {
    let recorded = record_path(folder);
    let matching: Vec<MappedFolder> = spaces
        .iter()
        .flat_map(|space| {
            space
                .folders
                .iter()
                .filter(|f| f.kind == folder_kinds::PROJECT && inside(&f.path, &recorded))
                .map(move |f| MappedFolder::of(space, f))
        })
        .collect();
    let Some(longest) = matching.iter().map(|f| f.path.len()).max() else {
        return Vec::new();
    };
    matching
        .into_iter()
        .filter(|f| f.path.len() == longest)
        .collect()
}

/// The patterns of a folder's rule (`include` or `exclude`).
pub fn rule_patterns<'a>(rules: &'a Value, key: &str) -> Vec<&'a str> {
    rules
        .get(key)
        .and_then(Value::as_array)
        .map(|patterns| patterns.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// Whether `path`, relative to a folder with `/` separators, or a folder it is in, matches a
/// pattern (section 15.4): `*` is any characters but `/`, `**` any number of folders, `?` one
/// character.
pub(crate) fn excluded(path: &str, patterns: &[&str]) -> bool {
    if path.is_empty() {
        return false;
    }
    let parts: Vec<&str> = path.split('/').collect();
    (1..=parts.len()).any(|n| {
        let prefix = parts[..n].join("/");
        patterns.iter().any(|pattern| glob(pattern, &prefix))
    })
}

/// Matches a whole path against a pattern of section 15.4.
pub(crate) fn glob(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.trim_matches('/').split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    match_parts(&pattern, &path)
}

fn match_parts(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| match_parts(rest, &path[skip..])),
        Some((first, rest)) => match path.split_first() {
            Some((part, others)) => match_part(first, part) && match_parts(rest, others),
            None => false,
        },
    }
}

fn match_part(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    // Classic wildcard matching with backtracking on the last `*`.
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((sp, st)) = star {
            p = sp + 1;
            t = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_follow_the_specification() {
        assert!(glob("node_modules/**", "node_modules"));
        assert!(glob("node_modules/**", "node_modules/a/b"));
        assert!(!glob("node_modules/**", "src/node_modules"));
        assert!(glob("**/node_modules/**", "src/node_modules/x"));
        assert!(glob("*.md", "notes.md"));
        assert!(!glob("*.md", "a/notes.md"));
        assert!(glob("**/*.md", "a/b/notes.md"));
        assert!(glob("**/*.md", "notes.md"));
        assert!(glob("drafts/?", "drafts/x"));
        assert!(!glob("drafts/?", "drafts/xy"));
        assert!(glob("a*b*c", "aXbYc"));
        assert!(!glob("a*b*c", "aXbY"));

        assert!(excluded("vendor/lib", &["vendor"]));
        assert!(excluded("tmp/scratch/deep", &["tmp/*"]));
        assert!(!excluded("src", &["vendor"]));
        assert!(!excluded("", &["**"]));
    }

    #[test]
    fn icons_are_one_emoji() {
        assert!(check_icon("🌱").is_ok());
        assert!(check_icon("👨‍👩‍👧").is_ok());
        assert!(check_icon("1️⃣").is_ok());
        assert!(check_icon("work").is_err());
        assert!(check_icon("x").is_err());
        assert!(check_icon("🌱 🌱").is_err());
        assert!(check_icon("é").is_err());
    }

    fn folder(id: &str, kind: &str, path: &str) -> Folder {
        let by = Provenance::person("Ada");
        Folder {
            id: id.into(),
            kind: kind.into(),
            path: path.into(),
            capture: true,
            rules: empty_object(),
            created_at: knode_format::Timestamp(0),
            created_by: by.clone(),
            updated_at: knode_format::Timestamp(0),
            updated_by: by,
        }
    }

    fn space(name: &str, folders: Vec<Folder>) -> SpaceFolders {
        SpaceFolders {
            space: PathBuf::from(format!("/k/{name}.knode")),
            name: name.into(),
            folders,
        }
    }

    #[test]
    fn overlapping_project_folders_are_found_once() {
        let spaces = [
            space(
                "Work",
                vec![
                    folder("w1", folder_kinds::PROJECT, "/code/site"),
                    folder("w2", folder_kinds::PROJECT, "/code/site/docs"),
                    folder("w3", folder_kinds::PROJECT, "/code/sitemap"),
                    // Notes and agent folders are not routed by project.
                    folder("w4", folder_kinds::NOTES, "/code/site/notes"),
                ],
            ),
            space(
                "Personal",
                vec![
                    folder("p1", folder_kinds::PROJECT, "/code/site"),
                    folder("p2", folder_kinds::PROJECT, "/code/site/blog"),
                    folder("p3", folder_kinds::PROJECT, "/elsewhere"),
                ],
            ),
        ];
        let found = folder_overlaps(&spaces);
        let pairs: Vec<(OverlapKind, &str, &str)> = found
            .iter()
            .map(|o| (o.kind, o.outer.folder.as_str(), o.inner.folder.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                (OverlapKind::Nested, "w1", "w2"),
                (OverlapKind::Same, "w1", "p1"),
                (OverlapKind::Nested, "w1", "p2"),
                (OverlapKind::Nested, "p1", "w2"),
                (OverlapKind::Nested, "p1", "p2"),
            ]
        );
        assert!(found[1].message.contains("captured into both"), "{found:?}");
        assert!(
            found[2]
                .message
                .contains("into \"Personal\" only, not into \"Work\"")
        );
        assert!(found[0].message.contains("both in \"Work\""));
        assert!(folder_overlaps(&spaces[1..]).len() == 1);
        assert!(folder_overlaps(&[]).is_empty());
    }

    #[test]
    fn a_folder_goes_to_its_nearest_project_folders() {
        let spaces = [
            space(
                "Work",
                vec![
                    folder("w1", folder_kinds::PROJECT, "/code/site"),
                    folder("w2", folder_kinds::PROJECT, "/code/site/docs"),
                ],
            ),
            space(
                "Personal",
                vec![folder("p1", folder_kinds::PROJECT, "/code/site")],
            ),
        ];
        let ids = |path: &str| -> Vec<String> {
            project_destinations(&spaces, Path::new(path))
                .into_iter()
                .map(|f| f.folder)
                .collect()
        };
        assert_eq!(ids("/code/site/docs/api"), ["w2"]);
        assert_eq!(ids("/code/site/src"), ["w1", "p1"]);
        assert_eq!(ids("/code/site"), ["w1", "p1"]);
        assert!(ids("/code/sitemap").is_empty());
        assert!(ids("/other").is_empty());
    }
}
