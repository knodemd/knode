//! The live Markdown folder of a space: the `markdown.folder` setting.
//!
//! When a space has the setting, Knode Service keeps the folder in step with it with a
//! [`LiveFolder`](knode_vault::LiveFolder): the whole space is written when the service opens
//! it, each change is written within a few seconds, and what is left is written when the service
//! stops. These functions read and change the setting; they work on an open space, so the CLI can
//! use them when the service cannot run.

use std::path::{Component, Path, PathBuf};

use knode_format::{Provenance, Space, setting_keys};

use crate::protocol::MarkdownFolder;

/// The live Markdown folder of `space`.
pub fn live_folder(space: &Space) -> knode_format::Result<MarkdownFolder> {
    let folder = space.setting(setting_keys::MARKDOWN_FOLDER)?;
    let path = folder.as_deref().and_then(resolve_path);
    let warning = path.as_deref().and_then(knode_vault::cloud_folder_warning);
    Ok(MarkdownFolder {
        folder,
        path,
        warning,
    })
}

/// Turns the live Markdown folder of `space` on at `folder`, an absolute path, or off with
/// `None`. Only a person can change it. Returns the folder as it is now, with a warning when it
/// is inside a cloud folder.
///
/// The folder may not exist yet; it may not be inside the space itself.
pub fn set_live_folder(
    space: &mut Space,
    folder: Option<&Path>,
    by: &Provenance,
) -> knode_format::Result<MarkdownFolder> {
    let value = match folder {
        None => None,
        Some(folder) => {
            if !folder.is_absolute() {
                return Err(knode_format::Error::Invalid(format!(
                    "the Markdown folder must be an absolute path: {}",
                    folder.display()
                )));
            }
            let folder = normalize(folder);
            if folder.starts_with(normalize(space.path())) {
                return Err(knode_format::Error::Invalid(format!(
                    "the Markdown folder cannot be inside the space: {}",
                    folder.display()
                )));
            }
            Some(record_path(&folder))
        }
    };
    space.set_setting(setting_keys::MARKDOWN_FOLDER, value.as_deref(), by)?;
    live_folder(space)
}

/// A path as the specification records the paths of folders (section 15.3): absolute and
/// normal, with `/` separators, relative to the home folder as `~/...` when it is inside it.
pub fn record_path(path: &Path) -> String {
    let path = normalize(path);
    if let Some(home) = home() {
        if let Ok(rest) = path.strip_prefix(&home) {
            let rest = slashes(rest);
            return if rest.is_empty() {
                "~".to_string()
            } else {
                format!("~/{rest}")
            };
        }
    }
    slashes(&path)
}

/// The folder on this machine for a path recorded with [`record_path`], or `None` for a path
/// that is neither absolute nor under `~`.
pub fn resolve_path(recorded: &str) -> Option<PathBuf> {
    let recorded = recorded.trim();
    if recorded == "~" {
        return home();
    }
    if let Some(rest) = recorded.strip_prefix("~/") {
        return home().map(|home| home.join(rest));
    }
    let path = PathBuf::from(recorded);
    path.is_absolute().then_some(path)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .map(|home| normalize(&home))
}

/// Removes `.` and `..` parts without following symbolic links.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_paths_as_the_specification_says() {
        let home = home().expect("tests run with HOME set");
        assert_eq!(record_path(&home.join("Notes/Work/")), "~/Notes/Work");
        assert_eq!(record_path(&home.join("Notes/./a/../Work")), "~/Notes/Work");
        assert_eq!(record_path(&home), "~");
        assert_eq!(
            record_path(Path::new("/Volumes/Work/notes")),
            "/Volumes/Work/notes"
        );
        assert_eq!(resolve_path("~/Notes"), Some(home.join("Notes")));
        assert_eq!(resolve_path("~"), Some(home));
        assert_eq!(
            resolve_path("/Volumes/Work"),
            Some(PathBuf::from("/Volumes/Work"))
        );
        assert_eq!(resolve_path("Notes"), None);
    }
}
