//! The name, settings and folders of a space: the shapes of `space.rename`, `settings.get`,
//! `settings.set`, `folders.list`, `folders.set` and `folders.remove`, and the rules they follow
//! (section 15 of the .knode specification).
//!
//! These functions work on an open space, so the CLI can use them when the service cannot run.
//! Only a person changes the name, the settings and the folders of a space.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use knode_format::{ActorKind, Folder, FolderSpec, Provenance, Space, folder_kinds, setting_keys};
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
}

impl FolderInfo {
    fn of(folder: Folder) -> Self {
        let local = resolve_path(&folder.path);
        let missing = !local.as_deref().is_some_and(Path::exists);
        FolderInfo {
            folder,
            local,
            missing,
        }
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
    Ok(space.folders()?.into_iter().map(FolderInfo::of).collect())
}

/// Adds a folder to the space, or changes one, and records the change. Only a person can
/// change folders.
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
    space.set_folder(&spec, by).map(FolderInfo::of)
}

/// Removes a folder from the space and records the change. Nothing captured or imported from
/// it is removed. Returns whether there was one. Only a person can change folders.
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
}
