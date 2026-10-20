//! Reading an app's SQLite database without touching it: the database (and its `-wal` and
//! `-shm` files, so recent changes are there) is copied to a temporary folder and the copy read.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use super::{ImportError, io_error};

/// A copy of a database, removed when dropped.
pub(crate) struct Copy {
    dir: PathBuf,
    pub conn: Connection,
}

impl Drop for Copy {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Copies the database at `path` and opens the copy.
pub(crate) fn open_copy(path: &Path) -> Result<Copy, ImportError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("knode-import-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).map_err(io_error(&dir))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "database.sqlite".into());
    let copy = dir.join(&name);
    let result = (|| {
        fs::copy(path, &copy).map_err(io_error(path))?;
        for suffix in ["-wal", "-shm"] {
            let side = PathBuf::from(format!("{}{suffix}", path.display()));
            if side.is_file() {
                fs::copy(&side, dir.join(format!("{name}{suffix}"))).map_err(io_error(&side))?;
            }
        }
        Connection::open(&copy).map_err(|e| ImportError::Unreadable {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    })();
    match result {
        Ok(conn) => Ok(Copy { dir, conn }),
        Err(e) => {
            let _ = fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

/// The columns of a table, empty when there is no such table.
pub(crate) fn columns(conn: &Connection, table: &str) -> BTreeSet<String> {
    let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info(\"{table}\")")) else {
        return BTreeSet::new();
    };
    stmt.query_map([], |row| row.get::<_, String>(1))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// The first of `names` the table has, as `alias.column`, else `NULL`: app versions name the
/// same column differently. Several present are `COALESCE`d in the order given.
pub(crate) fn pick(columns: &BTreeSet<String>, alias: &str, names: &[&str]) -> String {
    let present: Vec<String> = names
        .iter()
        .filter(|n| columns.contains(**n))
        .map(|n| format!("{alias}.{n}"))
        .collect();
    match present.len() {
        0 => "NULL".into(),
        1 => present[0].clone(),
        _ => format!("COALESCE({})", present.join(", ")),
    }
}

pub(crate) fn sql_error(path: &Path) -> impl Fn(rusqlite::Error) -> ImportError + '_ {
    move |e| ImportError::Unreadable {
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}
