//! The `knode.db` schema and its migrations.

use rusqlite::Connection;

use crate::{Error, Result};

/// Every migration, in order. Migration `n` (from 1) takes the database from schema version
/// `n - 1` to `n`; the schema version is SQLite's `user_version`.
const MIGRATIONS: &[&str] = &[include_str!("sql/0001-initial.sql")];

/// The schema version this library writes: the `user_version` of an up to date `knode.db`.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// Sets the connection options every `knode.db` connection uses.
pub(crate) fn configure(conn: &Connection) -> Result<()> {
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(Error::Invalid(format!(
            "knode.db could not be put in WAL mode (got {mode})"
        )));
    }
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;
         PRAGMA busy_timeout = 5000;",
    )?;
    Ok(())
}

/// The schema version of a database: its `user_version`.
pub(crate) fn version(conn: &Connection) -> Result<u32> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// Brings a database up to [`SCHEMA_VERSION`], one migration per transaction.
///
/// Fails with [`Error::NewerSchema`] when the database was written by a newer version.
pub(crate) fn migrate(conn: &mut Connection) -> Result<()> {
    let found = version(conn)?;
    if found > SCHEMA_VERSION {
        return Err(Error::NewerSchema {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(found as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", index as u32 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_an_empty_database_to_the_latest_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        assert_eq!(version(&conn).unwrap(), SCHEMA_VERSION);
        // Running it again changes nothing.
        migrate(&mut conn).unwrap();
        assert_eq!(version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn refuses_a_newer_database() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        assert!(matches!(migrate(&mut conn), Err(Error::NewerSchema { .. })));
    }
}
