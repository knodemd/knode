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

    /// Adds a folder, or changes one, and returns it. Only a person can change folders.
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

    /// Removes a folder. Nothing captured or imported from it is removed. Returns whether there
    /// was one. Only a person can change folders.
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
