//! The live Markdown folder of a space: the `markdown.folder` setting.
//!
//! When a space has the setting, Knode Service keeps the folder in step with it, both ways, with
//! a [`LiveFolder`](knode_vault::LiveFolder): when the service opens the space, the files edited
//! since it last wrote them are read in and then the whole space is written; each change made in
//! Knode is written within a few seconds; the folder is watched, and each file edited in it is
//! read in as an edit by the person the service runs for, with the file as its source; what is
//! left is written when the service stops. These functions read and change the setting; they
//! work on an open space, so the CLI can use them when the service cannot run.

use std::path::{Component, Path, PathBuf};

use knode_format::{NodeId, Provenance, Space, setting_keys};
use knode_sync::Docs;
use knode_vault::{LiveFolder, PullReport};

use crate::protocol::{DeletedNote, MarkdownFolder};

/// The notes of `space` whose file was deleted from its live Markdown folder, newest first,
/// with the path the file had. Each waits for a person to delete it or keep it
/// ([`keep_deleted`]). See [`knode_vault::property_keys::DELETED`].
pub fn deleted_notes(space: &Space) -> knode_format::Result<Vec<DeletedNote>> {
    let mut out = Vec::new();
    for summary in space.recent(usize::MAX)? {
        if let Some(path) = space.property(&summary.id, knode_vault::property_keys::DELETED)? {
            out.push(DeletedNote {
                id: summary.id,
                title: summary.title,
                path,
            });
        }
    }
    Ok(out)
}

/// Keeps a note whose file was deleted from the live Markdown folder: removes its
/// [`markdown.deleted`](knode_vault::property_keys::DELETED) property, so the folder writes its
/// file again once it hears of the change. Returns whether the note was marked.
pub fn keep_deleted(space: &mut Space, id: &NodeId) -> knode_format::Result<bool> {
    space.remove_property(id, knode_vault::property_keys::DELETED)
}

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

/// The person the changes read from a live folder are made as: the user the service runs for,
/// who owns the folder.
pub(crate) fn folder_owner() -> Provenance {
    let name = ["USER", "USERNAME"]
        .iter()
        .filter_map(|var| std::env::var(var).ok())
        .map(|name| name.trim().to_string())
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| "me".to_string());
    Provenance::person(name)
}

/// Opens the live folder `dir` of `space`: reads in the files edited since Knode last wrote them,
/// merging bodies through the service's `docs`, writes the whole space, then watches the folder. A folder that cannot be watched still
/// follows the space; its edits come in the next time the space is opened.
pub(crate) fn start_live_folder(
    space: &mut Space,
    dir: &Path,
    docs: &mut Docs,
) -> knode_vault::Result<(LiveFolder, PullReport)> {
    let mut live = LiveFolder::open(dir)?.with_source_root(record_path(dir));
    let report = live.start_with(space, &folder_owner(), docs)?;
    if let Err(e) = live.watch() {
        eprintln!(
            "Knode Service: cannot watch the Markdown folder {}: {e}",
            dir.display()
        );
    }
    Ok((live, report))
}

/// Says what a pull from a live folder did that a person should know: files deleted in the
/// folder, whose nodes wait for them, and files that could not be read.
pub(crate) fn log_pull(dir: &Path, report: &PullReport) {
    for path in &report.deleted {
        eprintln!(
            "Knode Service: {path} was deleted from {}; its node is kept and marked {}",
            dir.display(),
            knode_vault::property_keys::DELETED
        );
    }
    for skipped in &report.skipped {
        eprintln!(
            "Knode Service: {}: {}: {}",
            dir.display(),
            skipped.path,
            skipped.reason
        );
    }
}

/// A path as the specification records the paths of folders (section 15.3): absolute and
/// normal, in Unicode NFC, with `/` separators, relative to the home folder as `~/...` when it is inside it.
pub fn record_path(path: &Path) -> String {
    use unicode_normalization::UnicodeNormalization;
    let path: String = path.to_string_lossy().nfc().collect();
    let path = normalize(Path::new(&path));
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
        // Decomposed accents are recorded composed.
        assert_eq!(record_path(&home.join("Cafe\u{301}")), "~/Caf\u{e9}");
        assert_eq!(resolve_path("~/Notes"), Some(home.join("Notes")));
        assert_eq!(resolve_path("~"), Some(home));
        assert_eq!(
            resolve_path("/Volumes/Work"),
            Some(PathBuf::from("/Volumes/Work"))
        );
        assert_eq!(resolve_path("Notes"), None);
    }
}
