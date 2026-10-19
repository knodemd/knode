//! The SQLite state databases of editors built on VS Code (`state.vscdb`), read without getting
//! in the editor's way.
//!
//! The editor has the database open and writes to it all the time, so it is never written,
//! never locked against the editor, and never copied (Cursor's global database can grow to many
//! gigabytes):
//!
//! - it is opened through a `file:` URI with `mode=ro` and `PRAGMA query_only`, so nothing can be
//!   written to it;
//! - when a `-wal` file sits next to it, the database is in WAL mode: a read-only reader takes
//!   no lock that blocks the editor's writes, and reading through the WAL sees what the editor
//!   wrote last (with `immutable=1` the WAL would be ignored, and recent conversations missed);
//! - when there is no `-wal` file, it is also opened with `immutable=1`, which takes no lock at
//!   all, so a database in rollback-journal mode never makes the editor wait. A read caught in
//!   the middle of a write fails or reads stale data, and is simply done again at the next
//!   change.
//!
//! Each read runs in one read transaction, so it sees a single consistent state, and the
//! connection is closed as soon as the read is done. A read-only connection never checkpoints
//! the WAL.
//!
//! One database holds many conversations, so each conversation is known by a path of its own,
//! `<folder>/state.vscdb#<id>`.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row};

/// The database file name.
pub(crate) const DATABASE: &str = "state.vscdb";

/// The path that stands for one conversation in the database in `dir`.
pub(crate) fn session_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{DATABASE}#{id}"))
}

/// The database and conversation id a conversation path stands for.
pub(crate) fn split(path: &Path) -> Option<(PathBuf, String)> {
    let name = path.file_name()?.to_str()?;
    let id = name.strip_prefix(DATABASE)?.strip_prefix('#')?;
    (!id.is_empty()).then(|| (path.with_file_name(DATABASE), id.to_string()))
}

/// Whether a changed file is a state database or its WAL or journal (not a backup copy).
pub(crate) fn is_database_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix(DATABASE))
        .is_some_and(|rest| matches!(rest, "" | "-wal" | "-journal"))
}

/// Opens the database read-only, as described in the module documentation, and starts a read
/// transaction. A missing database is [`io::ErrorKind::NotFound`].
pub(crate) fn open(database: &Path) -> io::Result<Connection> {
    if !database.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} does not exist", database.display()),
        ));
    }
    let mut wal = database.as_os_str().to_owned();
    wal.push("-wal");
    let mut uri = format!("file:{}?mode=ro", uri_path(database));
    if !Path::new(&wal).exists() {
        uri.push_str("&immutable=1");
    }
    let connection = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(io::Error::other)?;
    connection
        .busy_timeout(Duration::from_secs(2))
        .map_err(io::Error::other)?;
    connection
        .execute_batch("PRAGMA query_only = ON; BEGIN;")
        .map_err(io::Error::other)?;
    Ok(connection)
}

/// Escapes a path for a `file:` URI, where `?` and `#` end the path and `%` starts an escape.
fn uri_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('%', "%25")
        .replace('?', "%3F")
        .replace('#', "%23")
}

/// Whether the database has a table with this name.
pub(crate) fn has_table(connection: &Connection, table: &str) -> io::Result<bool> {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |_| Ok(()),
        )
        .optional()
        .map(|found| found.is_some())
        .map_err(io::Error::other)
}

/// The value of `key` in the editor's `ItemTable`, if there is one.
pub(crate) fn item(connection: &Connection, key: &str) -> io::Result<Option<Vec<u8>>> {
    if !has_table(connection, "ItemTable")? {
        return Ok(None);
    }
    connection
        .query_row("SELECT value FROM ItemTable WHERE key = ?1", [key], |row| {
            Ok(bytes(row, 0))
        })
        .optional()
        .map_err(io::Error::other)
}

/// A column stored as text or as a blob, as bytes.
pub(crate) fn bytes(row: &Row<'_>, index: usize) -> Vec<u8> {
    match row.get_ref(index) {
        Ok(ValueRef::Text(bytes) | ValueRef::Blob(bytes)) => bytes.to_vec(),
        _ => Vec::new(),
    }
}

/// A 64-bit FNV-1a hash, used as the version of a stored value.
pub(crate) fn hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The folder of a `file://` URI, decoded.
pub(crate) fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let path = uri.strip_prefix("file://")?;
    Some(PathBuf::from(crate::text::percent_decode(path)))
}

/// Reads `workspace.json` in a folder of `workspaceStorage`: the folder (or workspace file)
/// the editor window had open.
pub(crate) fn workspace_folder(dir: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(dir.join("workspace.json")).ok()?;
    let workspace: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    ["folder", "workspace"].iter().find_map(|key| {
        let uri = workspace.get(*key)?.as_str()?;
        file_uri_path(uri)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_paths_round_trip() {
        let path = session_path(Path::new("/data/User/globalStorage"), "c1");
        assert_eq!(path, Path::new("/data/User/globalStorage/state.vscdb#c1"));
        assert_eq!(
            split(&path),
            Some((
                PathBuf::from("/data/User/globalStorage/state.vscdb"),
                "c1".into()
            ))
        );
        assert!(split(Path::new("/data/state.vscdb")).is_none());
        assert!(split(Path::new("/data/state.vscdb#")).is_none());
    }

    #[test]
    fn recognises_database_files() {
        assert!(is_database_file(Path::new("/d/state.vscdb")));
        assert!(is_database_file(Path::new("/d/state.vscdb-wal")));
        assert!(!is_database_file(Path::new("/d/state.vscdb.backup")));
        assert!(!is_database_file(Path::new("/d/state.vscdb#c1")));
        assert!(!is_database_file(Path::new("/d/other.db")));
    }

    #[test]
    fn escapes_uri_paths() {
        assert_eq!(uri_path(Path::new("/a b/c?#%")), "/a b/c%3F%23%25");
    }

    #[test]
    fn reads_file_uris() {
        assert_eq!(
            file_uri_path("file:///Users/example/my%20app"),
            Some(PathBuf::from("/Users/example/my app"))
        );
        assert_eq!(file_uri_path("vscode-remote://host/x"), None);
    }
}
