//! Whether the files a connector watches really changed.
//!
//! File events are not a reliable sign that a database changed. On macOS, FSEvents reports
//! events on a database and its `-wal` and `-shm` files when they are only read (as
//! [`super::sqlite::open_copy`] does), and reports writes to a file another app keeps open late,
//! often only when it is closed. [`FileStamp`] records what tells a real change: the size and
//! modification time of each file and of its `-wal`. Knode Service takes one before every run
//! of a connector with a [`super::Trigger::Watch`], and runs it on a file event only when the
//! stamp differs from the one of its last run.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The most entries of a watched folder a stamp records; a bigger folder cannot be stamped.
const MAX_ENTRIES: usize = 4096;

/// The size and modification time of a file, or `None` when it is not there.
type Meta = Option<(u64, SystemTime)>;

/// The size and modification time of watched files: each file and its `-wal`, and for a
/// folder the entries directly in it. Two stamps are equal when nothing in them changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStamp(Vec<(PathBuf, Meta)>);

impl FileStamp {
    /// The stamp of `paths` now. A file that is not there is recorded as missing. `None` when
    /// it cannot be told (a file cannot be read, or a folder is too big): the files are then
    /// taken as changed.
    pub fn of(paths: &[PathBuf]) -> Option<FileStamp> {
        let mut files = Vec::new();
        for path in paths {
            if path.is_dir() {
                let mut entries: Vec<PathBuf> = fs::read_dir(path)
                    .ok()?
                    .map(|e| e.map(|e| e.path()))
                    .collect::<io::Result<_>>()
                    .ok()?;
                if entries.len() > MAX_ENTRIES {
                    return None;
                }
                entries.sort();
                files.push((path.clone(), meta(path).ok()?));
                for entry in entries {
                    let m = meta(&entry).ok()?;
                    files.push((entry, m));
                }
            } else {
                files.push((path.clone(), meta(path).ok()?));
                let wal = wal(path);
                if !paths.contains(&wal) {
                    let m = meta(&wal).ok()?;
                    files.push((wal, m));
                }
            }
        }
        Some(FileStamp(files))
    }
}

/// The `-wal` file beside `path`.
fn wal(path: &Path) -> PathBuf {
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    PathBuf::from(wal)
}

fn meta(path: &Path) -> io::Result<Meta> {
    match fs::metadata(path) {
        // A folder's own size says nothing; its entries are stamped.
        Ok(m) if m.is_dir() => Ok(Some((0, m.modified()?))),
        Ok(m) => Ok(Some((m.len(), m.modified()?))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use rusqlite::Connection;

    use super::*;
    use crate::sources::sqlite::open_copy;

    #[test]
    fn reading_a_copy_leaves_the_stamp_alone_and_writing_changes_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("Synthetic.sqlite");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE chat (id INTEGER PRIMARY KEY, name TEXT);
             INSERT INTO chat (name) VALUES ('Example');",
        )
        .unwrap();
        let paths = vec![db.clone()];
        let before = FileStamp::of(&paths).unwrap();
        drop(open_copy(&db).unwrap());
        assert_eq!(FileStamp::of(&paths).unwrap(), before);
        // A row written by the app goes to the -wal.
        conn.execute("INSERT INTO chat (name) VALUES ('Another')", [])
            .unwrap();
        assert_ne!(FileStamp::of(&paths).unwrap(), before);
    }

    #[test]
    fn a_missing_file_and_a_folder_are_stamped() {
        let dir = tempfile::tempdir().unwrap();
        let missing = vec![dir.path().join("none.sqlite")];
        let empty = FileStamp::of(&missing).unwrap();
        fs::write(&missing[0], b"x").unwrap();
        assert_ne!(FileStamp::of(&missing).unwrap(), empty);

        let folder = vec![dir.path().to_path_buf()];
        let before = FileStamp::of(&folder).unwrap();
        assert_eq!(FileStamp::of(&folder).unwrap(), before);
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&missing[0])
            .unwrap();
        file.write_all(b"more").unwrap();
        assert_ne!(FileStamp::of(&folder).unwrap(), before);
    }
}
