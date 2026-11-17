//! Reading another app's SQLite database safely: from a copy, after checking its schema.
//!
//! Connectors to local apps (WhatsApp for Mac, Messages, Mail) never open the app's live
//! database and never write to it. [`open_copy`] copies the database with its `-wal` and `-shm`
//! files into a private temporary folder and opens the copy read-only, so the app is never
//! locked or changed; the copy is removed when it is dropped. [`check_schema`] then checks the
//! tables and columns the connector reads, and returns [`SourceError::NeedsUpdate`] when they
//! changed, before anything is read.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{Connection, OpenFlags};

use super::{Permission, SourceError};

/// A read-only copy of another app's database, removed when dropped.
#[derive(Debug)]
pub struct DatabaseCopy {
    conn: Option<Connection>,
    dir: PathBuf,
}

impl DatabaseCopy {
    /// The connection to the copy, opened read-only.
    pub fn conn(&self) -> &Connection {
        self.conn.as_ref().expect("open until dropped")
    }

    /// The folder holding the copy.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for DatabaseCopy {
    fn drop(&mut self) {
        drop(self.conn.take());
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Copies the database at `path` (with its `-wal` and `-shm` files when they are there) into a
/// private temporary folder and opens the copy read-only. The live file is only read, as bytes.
///
/// A missing database is [`SourceError::Unavailable`]; one macOS keeps Knode from reading is
/// [`SourceError::Permission`] with [`Permission::FullDiskAccess`].
pub fn open_copy(path: &Path) -> Result<DatabaseCopy, SourceError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "knode-source-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let name = path
        .file_name()
        .ok_or_else(|| SourceError::Unavailable(format!("{} is not a file", path.display())))?;
    let made = private_dir(&dir);
    if let Err(e) = made {
        return Err(SourceError::Failed(format!(
            "a temporary copy could not be made: {e}"
        )));
    }
    let mut copy = DatabaseCopy { conn: None, dir };
    let target = copy.dir.join(name);
    fs::copy(path, &target).map_err(|e| read_error(path, e))?;
    for suffix in ["-wal", "-shm"] {
        let mut side = path.as_os_str().to_os_string();
        side.push(suffix);
        let side = PathBuf::from(side);
        let mut to = target.as_os_str().to_os_string();
        to.push(suffix);
        match fs::copy(&side, PathBuf::from(to)) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(read_error(&side, e)),
        }
    }
    // Opened read-write first, so the copy takes in its -wal, then used read-only.
    let conn = Connection::open_with_flags(
        &target,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|conn| {
        conn.query_row("PRAGMA schema_version", [], |_| Ok(()))?;
        conn.execute_batch("PRAGMA query_only = ON;")?;
        Ok(conn)
    })
    .map_err(|e| {
        SourceError::NeedsUpdate(format!(
            "{} is not a database Knode reads: {e}",
            Path::new(name).display()
        ))
    })?;
    copy.conn = Some(conn);
    Ok(copy)
}

fn private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(dir)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(dir)
    }
}

fn read_error(path: &Path, error: io::Error) -> SourceError {
    match error.kind() {
        io::ErrorKind::NotFound => {
            SourceError::Unavailable(format!("{} is not on this machine", path.display()))
        }
        io::ErrorKind::PermissionDenied => SourceError::Permission(
            Permission::FullDiskAccess,
            format!(
                "macOS keeps Knode from reading {}: give Knode Full Disk Access in System \
                 Settings > Privacy & Security",
                path.display()
            ),
        ),
        _ => SourceError::Failed(format!("{} could not be read: {error}", path.display())),
    }
}

/// Checks that every table in `expected` is in the database with at least the columns listed.
/// Returns [`SourceError::NeedsUpdate`] naming what is missing otherwise.
///
/// ```
/// use knode_ingest::sources::sqlite::check_schema;
///
/// let conn = rusqlite::Connection::open_in_memory()?;
/// conn.execute_batch("CREATE TABLE message (ROWID INTEGER PRIMARY KEY, text TEXT, date INTEGER);")?;
/// assert!(check_schema(&conn, &[("message", &["ROWID", "text", "date"])]).is_ok());
/// assert!(check_schema(&conn, &[("message", &["attributedBody"])]).is_err());
/// # Ok::<(), rusqlite::Error>(())
/// ```
pub fn check_schema(conn: &Connection, expected: &[(&str, &[&str])]) -> Result<(), SourceError> {
    let mut missing: Vec<String> = Vec::new();
    for (table, columns) in expected {
        let found = table_columns(conn, table)
            .map_err(|e| SourceError::NeedsUpdate(format!("the database cannot be read: {e}")))?;
        if found.is_empty() {
            missing.push(format!("table {table}"));
            continue;
        }
        for column in *columns {
            if !found.iter().any(|c| c.eq_ignore_ascii_case(column)) {
                missing.push(format!("column {table}.{column}"));
            }
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(SourceError::NeedsUpdate(format!(
            "the app's database changed ({} missing)",
            missing.join(", ")
        )))
    }
}

fn table_columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info(?1)")?;
    let rows = stmt.query_map([table], |row| row.get::<_, String>(0))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic(dir: &Path) -> PathBuf {
        let path = dir.join("Synthetic.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE chat (id INTEGER PRIMARY KEY, name TEXT);
             INSERT INTO chat (name) VALUES ('Example');",
        )
        .unwrap();
        // Kept open, so the row is still in the -wal when the copy is made.
        std::mem::forget(conn);
        path
    }

    #[test]
    fn a_copy_is_read_and_the_original_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = synthetic(dir.path());
        let before = fs::read(&path).unwrap();
        let copy = open_copy(&path).unwrap();
        let name: String = copy
            .conn()
            .query_row("SELECT name FROM chat", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "Example");
        assert!(copy.conn().execute("DELETE FROM chat", []).is_err());
        check_schema(copy.conn(), &[("chat", &["id", "name"])]).unwrap();
        let err =
            check_schema(copy.conn(), &[("chat", &["id", "title"]), ("message", &[])]).unwrap_err();
        assert_eq!(err.kind(), "needs-update");
        assert!(err.to_string().contains("chat.title"));
        assert!(err.to_string().contains("table message"));
        let held = copy.dir().to_path_buf();
        drop(copy);
        assert!(!held.exists());
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn a_missing_database_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let err = open_copy(&dir.path().join("none.sqlite")).unwrap_err();
        assert_eq!(err.kind(), "unavailable");
    }
}
