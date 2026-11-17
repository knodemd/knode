//! Reading another app's SQLite database safely: from a copy, after checking its schema.
//!
//! Connectors to local apps (WhatsApp for Mac, Messages, Mail) never open the app's live
//! database and never write to it. [`open_copy`] copies the database with its `-wal` and `-shm`
//! files into a private temporary folder and opens the copy read-only, so the app is never
//! locked or changed; the copy is removed when it is dropped. A copy made while the app wrote
//! the files is made again, and one that is damaged is refused, so a connector never reads a
//! database and a `-wal` that do not match. [`check_schema`] then checks the
//! tables and columns the connector reads, and returns [`SourceError::NeedsUpdate`] when they
//! changed, before anything is read.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{Connection, OpenFlags};

use super::{FileStamp, Permission, SourceError};

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
    open_copy_with(path, &mut || {})
}

/// How many times a copy is made before giving up on a database that keeps changing.
const ATTEMPTS: u32 = 5;

/// [`open_copy`], calling `between` after the database is copied and before its `-wal` is, so
/// tests can change the files during a copy.
fn open_copy_with(path: &Path, between: &mut dyn FnMut()) -> Result<DatabaseCopy, SourceError> {
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
    // The database and its -wal are copied one after the other: if the app wrote or
    // checkpointed them meanwhile, the pair may not match. The copy is used only when their
    // sizes and modification times did not change while it was made.
    let mut settled = false;
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(50 * u64::from(attempt)));
        }
        let before = FileStamp::of(&[path.to_path_buf()]);
        copy_files(path, &target, between)?;
        let after = FileStamp::of(&[path.to_path_buf()]);
        if before.is_some() && before == after {
            settled = true;
            break;
        }
    }
    if !settled {
        return Err(SourceError::Failed(format!(
            "{} kept changing while it was copied; trying again later",
            Path::new(name).display()
        )));
    }
    // Opened read-write first, so the copy takes in its -wal, then used read-only.
    let conn = Connection::open_with_flags(
        &target,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|conn| {
        conn.query_row("PRAGMA schema_version", [], |_| Ok(()))?;
        Ok(conn)
    })
    .map_err(|e| {
        SourceError::NeedsUpdate(format!(
            "{} is not a database Knode reads: {e}",
            Path::new(name).display()
        ))
    })?;
    let check: String = conn
        .query_row("PRAGMA quick_check(1)", [], |r| r.get(0))
        .map_err(|e| {
            SourceError::Failed(format!(
                "the copy of {} could not be checked: {e}",
                Path::new(name).display()
            ))
        })?;
    if check != "ok" {
        return Err(SourceError::Failed(format!(
            "the copy of {} is damaged ({check}); trying again later",
            Path::new(name).display()
        )));
    }
    conn.execute_batch("PRAGMA query_only = ON;")
        .map_err(|e| SourceError::Failed(e.to_string()))?;
    copy.conn = Some(conn);
    Ok(copy)
}

/// Copies the database at `path` to `target`, then its `-wal` and `-shm` beside it. A side file
/// that is not there is removed from the copy too, so an earlier attempt's is never used.
fn copy_files(path: &Path, target: &Path, between: &mut dyn FnMut()) -> Result<(), SourceError> {
    fs::copy(path, target).map_err(|e| read_error(path, e))?;
    between();
    for suffix in ["-wal", "-shm"] {
        let mut side = path.as_os_str().to_os_string();
        side.push(suffix);
        let side = PathBuf::from(side);
        let mut to = target.as_os_str().to_os_string();
        to.push(suffix);
        let to = PathBuf::from(to);
        match fs::copy(&side, &to) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => match fs::remove_file(&to) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => {
                    return Err(SourceError::Failed(format!(
                        "a temporary copy could not be made: {e}"
                    )));
                }
                _ => {}
            },
            Err(e) => return Err(read_error(&side, e)),
        }
    }
    Ok(())
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

    fn append(path: &Path, bytes: &[u8]) {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(bytes).unwrap();
    }

    fn wal(path: &Path) -> PathBuf {
        PathBuf::from(format!("{}-wal", path.display()))
    }

    #[test]
    fn a_copy_made_while_the_files_changed_is_made_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = synthetic(dir.path());
        let mut writes = 0;
        // The app writes to the database once, while the first copy is made.
        let copy = open_copy_with(&path, &mut || {
            if writes == 0 {
                let conn = Connection::open(&path).unwrap();
                conn.execute("INSERT INTO chat (name) VALUES ('Later')", [])
                    .unwrap();
                std::mem::forget(conn);
            }
            writes += 1;
        })
        .unwrap();
        assert!(writes >= 2);
        let names: i64 = copy
            .conn()
            .query_row("SELECT COUNT(*) FROM chat", [], |r| r.get(0))
            .unwrap();
        assert_eq!(names, 2);
    }

    #[test]
    fn a_database_that_never_settles_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = synthetic(dir.path());
        let wal = wal(&path);
        let err = open_copy_with(&path, &mut || {
            // Every copy, the -wal grows between the database and its copy.
            append(&wal, &[0; 512]);
        })
        .unwrap_err();
        assert_eq!(err.kind(), "failed");
        assert!(err.to_string().contains("kept changing"), "{err}");
    }

    #[test]
    fn a_damaged_copy_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Damaged.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "PRAGMA page_size = 4096;
                 CREATE TABLE chat (id INTEGER PRIMARY KEY, name TEXT);
                 CREATE INDEX chat_name ON chat (name);",
            )
            .unwrap();
            for i in 0..200 {
                conn.execute(
                    "INSERT INTO chat (name) VALUES (?1)",
                    [format!("n{i}-{}", "x".repeat(100))],
                )
                .unwrap();
            }
        }
        // Junk over the pages after the first, where the tables are.
        let mut bytes = fs::read(&path).unwrap();
        assert!(bytes.len() > 3 * 4096);
        for b in &mut bytes[4096 + 100..3 * 4096] {
            *b = 0xA5;
        }
        fs::write(&path, bytes).unwrap();
        let err = open_copy(&path).unwrap_err();
        assert_eq!(err.kind(), "failed", "{err}");
    }

    #[test]
    fn a_missing_database_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let err = open_copy(&dir.path().join("none.sqlite")).unwrap_err();
        assert_eq!(err.kind(), "unavailable");
    }
}
