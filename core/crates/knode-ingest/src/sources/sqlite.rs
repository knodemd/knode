//! Reading another app's SQLite database safely: from a copy, after checking its schema.
//!
//! Connectors to local apps (WhatsApp for Mac, Messages, Mail) never open the app's live
//! database and never write to it. [`open_copy`] copies the database with its `-wal` and `-shm`
//! files into a private temporary folder and opens the copy read-only, so the app is never
//! locked or changed; the copy is removed when it is dropped. A copy made while the app wrote
//! the files is made again, so a connector never reads a database and a `-wal` that do not
//! match, and a copy that had to be made again is checked for damage before it is used.
//! [`check_schema`] then checks the tables and columns the connector reads, and returns
//! [`SourceError::NeedsUpdate`] when they changed, before anything is read. Copies left behind
//! by a process that ended without removing them are removed with [`remove_stale_copies`].
//! [`Reused`] keeps a copy for the rest of a run while the database does not change, so a run
//! of many batches copies it once.

use std::fmt;
use std::fs;
use std::io;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

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
    let name = path
        .file_name()
        .ok_or_else(|| SourceError::Unavailable(format!("{} is not a file", path.display())))?;
    let dir = copy_dir(&std::env::temp_dir())
        .map_err(|e| SourceError::Failed(format!("a temporary copy could not be made: {e}")))?;
    let mut copy = DatabaseCopy { conn: None, dir };
    let target = copy.dir.join(name);
    // The database and its -wal are copied one after the other: if the app wrote or
    // checkpointed them meanwhile, the pair may not match. The copy is used only when their
    // sizes and modification times did not change while it was made.
    let mut settled = false;
    let mut retried = false;
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            retried = true;
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
    .map_err(|e| open_error(Path::new(name), &e))?;
    // A copy made while the app wrote its files was made again: it is checked, so a database
    // and a -wal caught in the middle of a write are never read. A copy made in one go needs
    // no check, and a database of a gigabyte is not read through on every run.
    if retried {
        let check: String = conn
            .query_row("PRAGMA quick_check(1)", [], |r| r.get(0))
            .map_err(|e| open_error(Path::new(name), &e))?;
        if check != "ok" {
            return Err(SourceError::Failed(format!(
                "the copy of {} is damaged ({check}); trying again later",
                Path::new(name).display()
            )));
        }
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

/// What an error opening or checking the copy of `name` means: a file that is not a database
/// (or not one this SQLite reads) needs an update of the connector; a damaged copy, or an
/// error of the disk, is tried again later.
fn open_error(name: &Path, error: &rusqlite::Error) -> SourceError {
    use rusqlite::ErrorCode;
    match error.sqlite_error_code() {
        Some(ErrorCode::NotADatabase) => SourceError::NeedsUpdate(format!(
            "{} is not a database Knode reads: {error}",
            name.display()
        )),
        Some(ErrorCode::DatabaseCorrupt) => SourceError::Failed(format!(
            "the copy of {} is damaged ({error}); trying again later",
            name.display()
        )),
        _ => SourceError::Failed(format!(
            "the copy of {} could not be opened: {error}",
            name.display()
        )),
    }
}

/// The start of the name of the temporary folder of a copy: `knode-source-<pid>-<n>`.
const COPY_PREFIX: &str = "knode-source-";

/// Removes the temporary copies, in the system's temporary folder, left by processes that are
/// no longer running (one that crashed, or was killed, while it read a copy). `running` tells
/// whether a process id is running; copies of this process are always kept. Returns how many
/// were removed.
pub fn remove_stale_copies(running: impl Fn(u32) -> bool) -> usize {
    remove_stale_copies_in(&std::env::temp_dir(), running)
}

fn remove_stale_copies_in(temp: &Path, running: impl Fn(u32) -> bool) -> usize {
    let Ok(entries) = fs::read_dir(temp) else {
        return 0;
    };
    let me = std::process::id();
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix(COPY_PREFIX))
            .and_then(|rest| rest.split_once('-'))
            .filter(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|(pid, _)| pid.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == me || running(pid) {
            continue;
        }
        // Only folders: a copy is always one, and a symbolic link is never followed.
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if is_dir && fs::remove_dir_all(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Makes a new private folder for a copy in `temp`: `knode-source-<pid>-<n>`. A folder of that
/// name left by an earlier process with the same id (one that crashed before its copies were
/// removed) is never reused: the next number is tried.
fn copy_dir(temp: &Path) -> io::Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let mut tries = 0;
    loop {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = temp.join(format!("{COPY_PREFIX}{pid}-{n}"));
        match private_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && tries < 1000 => tries += 1,
            Err(e) => return Err(e),
        }
    }
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

/// A copy (or anything made from one, such as a checked snapshot) kept while the files it was
/// made from do not change, so the batches of one run read the same copy instead of copying a
/// database of a gigabyte for each one.
///
/// A connector keeps one, takes its copy with [`Reused::get`] and lets it go with
/// [`Reused::release`] when the run ends, so no copy of another app's data stays on disk
/// between runs. Clones share the copy.
pub struct Reused<T> {
    held: Arc<Mutex<Kept<T>>>,
    made: Arc<AtomicU64>,
}

impl<T> Reused<T> {
    /// The copy kept, when the files are still as `stamp` says they were when it was made;
    /// otherwise the one kept is removed first and a new one is made with `take` and kept. A
    /// copy made while the files could not be told (`stamp` is `None`) is never reused.
    ///
    /// The copy stays locked while the guard returned lives.
    pub fn get<E>(
        &self,
        stamp: Option<FileStamp>,
        take: impl FnOnce() -> Result<T, E>,
    ) -> Result<Held<'_, T>, E> {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        let fresh =
            matches!((&stamp, held.as_ref()), (Some(now), Some((Some(then), _))) if now == then);
        if !fresh {
            // Removed before the new copy is made, so two are never on disk at once.
            *held = None;
            let value = take()?;
            self.made.fetch_add(1, Ordering::Relaxed);
            *held = Some((stamp, value));
        }
        Ok(Held(held))
    }

    /// Removes the copy kept, if any.
    pub fn release(&self) {
        let taken = self.held.lock().unwrap_or_else(|e| e.into_inner()).take();
        drop(taken);
    }

    /// How many copies were made so far.
    pub fn made(&self) -> u64 {
        self.made.load(Ordering::Relaxed)
    }

    /// Whether a copy is kept now.
    pub fn is_held(&self) -> bool {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }
}

impl<T> Default for Reused<T> {
    fn default() -> Self {
        Reused {
            held: Arc::new(Mutex::new(None)),
            made: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl<T> Clone for Reused<T> {
    fn clone(&self) -> Self {
        Reused {
            held: Arc::clone(&self.held),
            made: Arc::clone(&self.made),
        }
    }
}

impl<T> fmt::Debug for Reused<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never waits: it may be printed while the copy is held.
        let held = self.held.try_lock().map(|h| h.is_some()).ok();
        f.debug_struct("Reused").field("held", &held).finish()
    }
}

/// What a [`Reused`] keeps: the copy, with the files as they were when it was made.
type Kept<T> = Option<(Option<FileStamp>, T)>;

/// The copy kept by a [`Reused`], locked while this lives.
pub struct Held<'a, T>(MutexGuard<'a, Kept<T>>);

impl<T> Deref for Held<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0.as_ref().expect("kept while held").1
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
        let found = table_columns(conn, table).map_err(|e| match e.sqlite_error_code() {
            Some(rusqlite::ErrorCode::NotADatabase) => {
                SourceError::NeedsUpdate(format!("the database cannot be read: {e}"))
            }
            _ => SourceError::Failed(format!("the database could not be read: {e}")),
        })?;
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

    /// A synthetic database whose pages after the first, where the tables are, are junk.
    fn damaged(dir: &Path) -> PathBuf {
        let path = dir.join("Damaged.sqlite");
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
        path
    }

    #[test]
    fn a_damaged_copy_made_again_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = damaged(dir.path());
        let mut writes = 0;
        // The "app" touches the database once while the first copy is made: the copy is made
        // again, and checked.
        let err = open_copy_with(&path, &mut || {
            if writes == 0 {
                let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
                fs::File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(later)
                    .unwrap();
            }
            writes += 1;
        })
        .unwrap_err();
        assert!(writes >= 2);
        assert_eq!(err.kind(), "failed", "{err}");
        assert!(err.to_string().contains("damaged"), "{err}");
    }

    #[test]
    fn a_copy_made_in_one_go_is_not_read_through() {
        let dir = tempfile::tempdir().unwrap();
        // Damaged past its first page: only a full check would see it, and none is made.
        let path = damaged(dir.path());
        let copy = open_copy(&path).unwrap();
        // Reading the damaged table fails as an error to try again, not as a schema change.
        let err = copy
            .conn()
            .query_row("SELECT COUNT(*) FROM chat WHERE name > ''", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_err();
        assert_eq!(
            err.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseCorrupt)
        );
    }

    #[test]
    fn only_a_file_that_is_not_a_database_needs_an_update() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("NotADatabase.sqlite");
        fs::write(&path, vec![0x5A; 8192]).unwrap();
        let err = open_copy(&path).unwrap_err();
        assert_eq!(err.kind(), "needs-update", "{err}");

        let name = Path::new("Synthetic.sqlite");
        let io = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
            None,
        );
        assert_eq!(open_error(name, &io).kind(), "failed");
        let cannot_open = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
            None,
        );
        assert_eq!(open_error(name, &cannot_open).kind(), "failed");
        let corrupt = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            None,
        );
        assert_eq!(open_error(name, &corrupt).kind(), "failed");
    }

    #[test]
    fn copies_of_processes_that_ended_are_removed() {
        let temp = tempfile::tempdir().unwrap();
        let me = std::process::id();
        let ended = temp.path().join(format!("{COPY_PREFIX}4000001-0"));
        let running = temp.path().join(format!("{COPY_PREFIX}4000002-3"));
        let mine = temp.path().join(format!("{COPY_PREFIX}{me}-7"));
        let other = temp.path().join(format!("{COPY_PREFIX}notes"));
        for dir in [&ended, &running, &mine, &other] {
            fs::create_dir(dir).unwrap();
            fs::write(dir.join("Synthetic.sqlite"), b"x").unwrap();
        }
        let file = temp.path().join(format!("{COPY_PREFIX}4000003-1"));
        fs::write(&file, b"x").unwrap();
        let removed = remove_stale_copies_in(temp.path(), |pid| pid == 4000002);
        assert_eq!(removed, 1);
        assert!(!ended.exists());
        assert!(running.exists());
        assert!(mine.exists());
        assert!(other.exists());
        assert!(file.exists(), "only folders are removed");
    }

    #[test]
    fn a_missing_database_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let err = open_copy(&dir.path().join("none.sqlite")).unwrap_err();
        assert_eq!(err.kind(), "unavailable");
    }
    #[test]
    fn a_folder_left_with_the_same_process_id_is_not_reused() {
        let temp = tempfile::tempdir().unwrap();
        let me = std::process::id();
        // Folders an earlier process with this id left behind: every next number is taken.
        let first = copy_dir(temp.path()).unwrap();
        let next: u64 = first
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.rsplit('-').next())
            .and_then(|n| n.parse().ok())
            .unwrap();
        for n in next + 1..next + 4 {
            fs::create_dir(temp.path().join(format!("{COPY_PREFIX}{me}-{n}"))).unwrap();
        }
        let dir = copy_dir(temp.path()).unwrap();
        assert_ne!(dir, first);
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            0,
            "a new, empty folder"
        );
    }

    #[test]
    fn a_copy_is_reused_until_the_database_changes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Synthetic.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE t (x INTEGER); INSERT INTO t VALUES (1);")
            .unwrap();
        drop(conn);
        let stamp = || FileStamp::of(std::slice::from_ref(&path));
        let reused: Reused<DatabaseCopy> = Reused::default();
        let copies = std::cell::Cell::new(0);
        let take = || {
            copies.set(copies.get() + 1);
            open_copy(&path)
        };
        let first = reused.get(stamp(), take).unwrap().dir().to_path_buf();
        let again = reused.get(stamp(), take).unwrap().dir().to_path_buf();
        assert_eq!(first, again);
        assert_eq!(
            copies.get(),
            1,
            "copied once while the database did not change"
        );

        // Changed: the old copy is removed and a new one made.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("INSERT INTO t VALUES (2); INSERT INTO t VALUES (3);")
            .unwrap();
        drop(conn);
        let held = reused.get(stamp(), take).unwrap();
        let n: i64 = held
            .conn()
            .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 3);
        assert!(!first.exists(), "the old copy is gone");
        let last = held.dir().to_path_buf();
        drop(held);
        assert_eq!(copies.get(), 2);

        // Not told: never reused.
        reused.get(None, take).unwrap();
        reused.get(None, take).unwrap();
        assert_eq!(copies.get(), 4);
        assert!(!last.exists());

        // Released: nothing stays on disk.
        let dir = reused.get(stamp(), take).unwrap().dir().to_path_buf();
        assert!(reused.is_held());
        reused.release();
        assert!(!reused.is_held());
        assert!(!dir.exists());
    }
}
