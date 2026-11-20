//! The settings and folders of a space (section 15 of the specification), and the spaces Knode
//! knows in `config.toml`.
//!
//! The apps change these through Knode Service while it runs. These functions apply the same
//! rules as the service (`knode_service::spaces`) when it does not, so a space changed here
//! looks the same as one changed through the service.

use std::path::Path;

use knode_service::spaces as rules;

use crate::{KnodeError, KnodeSpace, Provenance, Result, config_error, config_paths};

/// A folder that feeds a space: a project whose conversations are captured into it, a folder of
/// notes, or a folder where an agent keeps its sessions.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SpaceFolder {
    /// The folder's id, a ULID.
    pub id: String,
    /// `project`, `notes` or `agent`; kinds this version does not know are kept as they are.
    pub kind: String,
    /// The folder as the space records it: `~/...` inside the home folder, else absolute.
    pub path: String,
    /// Whether the folder is captured (a project) or read (notes, agent).
    pub capture: bool,
    /// The rules of the folder, a JSON object, such as `{"agent":"claude-code"}`.
    pub rules: String,
    /// The folder on this machine, or `None` when the path cannot be resolved here.
    pub local: Option<String>,
    /// Whether the folder does not exist on this machine. It is kept and skipped.
    pub missing: bool,
    /// When it was added, ms UTC.
    pub created_at: i64,
    /// When it last changed, ms UTC.
    pub updated_at: i64,
    /// For a project folder, the id of the `project` node named after it that it holds the
    /// files of. `None` for other folders, and for a project folder added before Knode made
    /// these.
    pub project_id: Option<String>,
    /// The title of that project node.
    pub project_title: Option<String>,
    /// Whether adding the folder just created that project node, rather than finding the one
    /// the space had. Always `false` in a list of folders.
    pub project_created: bool,
    /// Whether the folder had been added before and already held that project. Always `false`
    /// in a list of folders.
    pub project_already_held: bool,
    /// Whether the folder had been removed and, added again, took back the project it held
    /// before. Always `false` in a list of folders.
    pub project_held_before: bool,
    /// When adding the folder gave it a project named after its parent folders too, such as
    /// `api (b)`, the title of the project of its own name that another folder holds.
    pub project_passed_over: Option<String>,
    /// The folders, as recorded, that hold the files of that passed-over project, or, when
    /// `project_passed_over_released`, the folder that held them last.
    pub project_passed_over_folders: Vec<String>,
    /// Whether no folder holds the passed-over project now: it is kept for the folder that
    /// held it.
    pub project_passed_over_released: bool,
    /// The other folders, as recorded, that hold the files of this folder's project too. Empty
    /// in a list of folders.
    pub project_shared_with: Vec<String>,
}

impl From<knode_service::FolderInfo> for SpaceFolder {
    fn from(info: knode_service::FolderInfo) -> Self {
        SpaceFolder {
            id: info.folder.id,
            kind: info.folder.kind,
            path: info.folder.path,
            capture: info.folder.capture,
            rules: info.folder.rules.to_string(),
            local: info.local.map(|p| p.display().to_string()),
            missing: info.missing,
            created_at: info.folder.created_at.0,
            updated_at: info.folder.updated_at.0,
            project_id: info.project.as_ref().map(|p| p.node.id.to_string()),
            project_title: info.project.as_ref().map(|p| p.node.title.clone()),
            project_created: info.project.as_ref().is_some_and(|p| p.created),
            project_already_held: info.project.as_ref().is_some_and(|p| p.already_held),
            project_held_before: info.project.as_ref().is_some_and(|p| p.held_before),
            project_passed_over: info
                .project
                .as_ref()
                .and_then(|p| p.passed_over.as_ref().map(|o| o.title.clone())),
            project_passed_over_folders: info
                .project
                .as_ref()
                .and_then(|p| p.passed_over.as_ref().map(|o| o.folders.clone()))
                .unwrap_or_default(),
            project_passed_over_released: info
                .project
                .as_ref()
                .and_then(|p| p.passed_over.as_ref())
                .is_some_and(|o| o.released),
            project_shared_with: info.project.map(|p| p.shared_with).unwrap_or_default(),
        }
    }
}

/// A folder to add to a space, or to change.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FolderDraft {
    /// The folder to change, or `None` to add one (or change the one with the same kind and
    /// path).
    pub id: Option<String>,
    /// `project`, `notes` or `agent`.
    pub kind: String,
    /// The folder: an absolute path, or one starting with `~/`.
    pub path: String,
    /// Whether the folder is captured or read.
    pub capture: bool,
    /// The rules, a JSON object; `agent` is required for an agent folder.
    pub rules: String,
}

/// An agent whose conversations Knode captures, for the `agent` rule of an agent folder.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CaptureAgent {
    /// Its name in provenance and rules, such as `claude-code`.
    pub id: String,
    /// Its name for people, such as `Claude Code`.
    pub name: String,
}

/// The title of the project a project folder at `path` (absolute) holds, as adding it names the
/// project: the folder's name in NFC, with `[`, `]` and `|` replaced. `None` for the home folder
/// and the root of a disk, which add no project.
#[uniffi::export]
pub fn folder_project_title(path: String) -> Option<String> {
    let recorded = knode_service::markdown::record_path(Path::new(path.trim()));
    knode_format::folder_project_name(&recorded)
}

/// The agents capture reads, in the order Knode lists them.
#[uniffi::export]
pub fn capture_agents() -> Vec<CaptureAgent> {
    knode_service::Agent::ALL
        .iter()
        .map(|agent| CaptureAgent {
            id: agent.id().to_string(),
            name: agent.name().to_string(),
        })
        .collect()
}

#[uniffi::export]
impl KnodeSpace {
    /// Renames the space with the rules of Knode Service: the name is trimmed and may not hold
    /// control characters. Only a person can rename a space.
    pub fn rename_as(&self, name: String, by: Provenance) -> Result<crate::SpaceInfo> {
        let info = rules::rename(&mut self.lock(), &name, &by.into())?;
        Ok(crate::SpaceInfo {
            id: info.id,
            name: info.name,
            path: info.path.display().to_string(),
        })
    }

    /// Changes `space.icon` (one emoji) or `capture.enabled` (`on` or `off`), or puts it back
    /// to its default with `None`, with the rules of Knode Service. Returns whether anything
    /// changed. Only a person can change settings.
    pub fn change_setting(
        &self,
        key: String,
        value: Option<String>,
        by: Provenance,
    ) -> Result<bool> {
        Ok(rules::set_setting(
            &mut self.lock(),
            &key,
            value.as_deref(),
            &by.into(),
        )?)
    }

    /// Whether captured conversations are written to the space: its `capture.enabled`.
    pub fn capture_enabled(&self) -> Result<bool> {
        Ok(rules::capture_enabled(&self.lock())?)
    }

    /// The folders that feed the space, oldest first.
    pub fn folders(&self) -> Result<Vec<SpaceFolder>> {
        let folders = rules::folders(&self.lock())?;
        Ok(folders.into_iter().map(SpaceFolder::from).collect())
    }

    /// Adds a folder, or changes one, and returns it. Adding a project folder also creates the
    /// `project` node named after it, or finds the one the space has; a project another folder
    /// of the same name holds is not joined, a new one named after the parent folder is made
    /// instead. Only a person can change folders.
    pub fn set_folder(&self, folder: FolderDraft, by: Provenance) -> Result<SpaceFolder> {
        let rules_value: serde_json::Value = if folder.rules.trim().is_empty() {
            serde_json::Value::Object(Default::default())
        } else {
            serde_json::from_str(&folder.rules).map_err(|e| KnodeError::Invalid {
                message: format!("the rules of a folder are a JSON object: {e}"),
            })?
        };
        let input = knode_service::FolderInput {
            id: folder.id,
            kind: folder.kind,
            path: folder.path,
            capture: folder.capture,
            rules: rules_value,
        };
        let info = rules::set_folder(&mut self.lock(), &input, &by.into())?;
        Ok(info.into())
    }

    /// Removes a folder. Nothing captured or imported from it is removed, and a project folder's
    /// `project` node stays. Returns whether there was one. Only a person can change folders.
    pub fn remove_folder(&self, id: String, by: Provenance) -> Result<bool> {
        Ok(rules::remove_folder(&mut self.lock(), &id, &by.into())?)
    }
}

/// Every space Knode knows (`default_space`, then `spaces` in `config.toml`): Knode Service
/// routes captured conversations over their folders.
#[uniffi::export]
pub fn known_spaces() -> Result<Vec<String>> {
    known_spaces_in(&config_paths()?)
}

/// Whether capture routes conversations over the folders of this known space: it exists and is
/// not an archived copy, as `knode folders overlaps` decides.
#[uniffi::export]
pub fn is_routed_space(path: String) -> bool {
    knode_config::is_routed_space(Path::new(&path))
}

/// Adds a space to `spaces` in `config.toml` unless Knode knows it already. Returns whether it
/// was added.
#[uniffi::export]
pub fn remember_space(path: String) -> Result<bool> {
    remember_space_in(&config_paths()?, &path)
}

/// Takes a space out of `config.toml`: out of `spaces`, and no longer the default space if it
/// was. Its files are not touched. Returns whether Knode knew it.
#[uniffi::export]
pub fn forget_space(path: String) -> Result<bool> {
    forget_space_in(&config_paths()?, &path)
}

pub(crate) fn known_spaces_in(paths: &knode_config::Paths) -> Result<Vec<String>> {
    let config = knode_config::Config::load(paths).map_err(config_error)?;
    Ok(config
        .known_spaces()
        .iter()
        .map(|p| p.display().to_string())
        .collect())
}

pub(crate) fn remember_space_in(paths: &knode_config::Paths, path: &str) -> Result<bool> {
    let path = space_path(path)?;
    let mut config = knode_config::Config::load(paths).map_err(config_error)?;
    let added = config.remember_space(path);
    if added {
        config.save(paths).map_err(config_error)?;
    }
    Ok(added)
}

pub(crate) fn forget_space_in(paths: &knode_config::Paths, path: &str) -> Result<bool> {
    let path = space_path(path)?;
    let mut config = knode_config::Config::load(paths).map_err(config_error)?;
    let known = config.known_spaces().iter().any(|p| p == path);
    config.spaces.retain(|p| p != path);
    if config.default_space.as_deref() == Some(path) {
        config.default_space = None;
    }
    if known {
        config.save(paths).map_err(config_error)?;
    }
    Ok(known)
}

fn space_path(path: &str) -> Result<&Path> {
    let path = Path::new(path.trim_end_matches('/'));
    if !path.is_absolute() {
        return Err(KnodeError::Invalid {
            message: format!(
                "a space is named by its absolute path, not {}",
                path.display()
            ),
        });
    }
    Ok(path)
}

#[cfg(test)]
#[path = "spaces_tests.rs"]
mod tests;
