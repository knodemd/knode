//! The files of a node's Loro document in `sync/nodes/<id>/` (21.2).

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The full document, written by compaction.
pub const SNAPSHOT_FILE: &str = "snapshot.loro";
/// The folder of update files, one per write.
pub const UPDATES_DIR: &str = "updates";
/// The extension of update files.
pub const EXTENSION: &str = "loro";

/// What identifies the content of a snapshot file without reading it.
pub(crate) type Stamp = (u64, Option<SystemTime>);

pub(crate) fn snapshot_path(dir: &Path) -> PathBuf {
    dir.join(SNAPSHOT_FILE)
}

pub(crate) fn updates_dir(dir: &Path) -> PathBuf {
    dir.join(UPDATES_DIR)
}

pub(crate) fn stamp(path: &Path) -> io::Result<Option<Stamp>> {
    match fs::metadata(path) {
        Ok(meta) => Ok(Some((meta.len(), meta.modified().ok()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The names of the update files, `<ULID>.loro`: names starting with `.` (files being written)
/// and any other name are ignored.
pub(crate) fn update_names(dir: &Path) -> io::Result<Vec<(String, u64)>> {
    let entries = match fs::read_dir(updates_dir(dir)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".loro") else {
            continue;
        };
        if ulid::Ulid::from_string(stem).is_err() {
            continue;
        }
        let meta = entry.metadata()?;
        if meta.is_file() {
            names.push((name, meta.len()));
        }
    }
    names.sort();
    Ok(names)
}

/// Writes an update file under a new ULID and returns its name.
pub(crate) fn write_update(dir: &Path, bytes: &[u8]) -> io::Result<String> {
    let updates = updates_dir(dir);
    fs::create_dir_all(&updates)?;
    let name = format!("{}.{EXTENSION}", ulid::Ulid::generate());
    write_atomically(&updates, &name, bytes)?;
    Ok(name)
}

/// Writes `bytes` to a temporary name starting with `.`, flushes it to disk, and renames it
/// into place (21.2).
///
/// The folder is not flushed after the rename: a rename lost in a crash loses a file whose edits
/// SQLite either lacks too or holds, and 21.5 brings the document in step with SQLite then.
pub(crate) fn write_atomically(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let temp = dir.join(format!(".{name}.{}", ulid::Ulid::generate()));
    let result = (|| {
        let mut file = File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, dir.join(name))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}
