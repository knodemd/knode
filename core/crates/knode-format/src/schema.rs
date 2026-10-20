//! The `knode.db` schema and its migrations.

use rusqlite::Connection;

use crate::{Error, Result};

/// Every migration, in order. Migration `n` (from 1) takes the database from schema version
/// `n - 1` to `n`; the schema version is SQLite's `user_version`.
const MIGRATIONS: &[&str] = &[
    include_str!("sql/0001-initial.sql"),
    include_str!("sql/0002-properties.sql"),
    include_str!("sql/0003-v02.sql"),
    include_str!("sql/0004-v03.sql"),
];

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

    #[test]
    fn migrates_a_version_1_database_to_the_latest_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        migrate(&mut conn).unwrap();
        assert_eq!(version(&conn).unwrap(), SCHEMA_VERSION);
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name = 'properties'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1);
    }

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
            == 1
    }

    #[test]
    fn a_fresh_database_has_every_table_of_version_4() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        assert_eq!(SCHEMA_VERSION, 4);
        for table in [
            "settings",
            "provenance",
            "nodes",
            "tags",
            "links",
            "unresolved_links",
            "history",
            "nodes_fts",
            "properties",
            "folders",
            "agents",
            "agent_keys",
            "grants",
            "changes",
            "link_history",
            "link_kinds",
            "views",
            "positions",
            "view_groups",
            "embedding_models",
            "embedding_chunks",
            "blobs",
            "attachments",
            "loro_docs",
            "loro_peers",
        ] {
            assert!(table_exists(&conn, table), "{table} is missing");
        }
        let columns: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('history') ORDER BY cid")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(columns[columns.len() - 2..], ["change_id", "restored_from"]);
    }

    #[test]
    fn migrates_a_version_2_database_making_every_version_a_change() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute_batch(MIGRATIONS[1]).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        conn.execute_batch(
            "INSERT INTO provenance (id, actor_kind, actor) VALUES (1, 'person', 'Ada');
             INSERT INTO provenance (id, actor_kind, actor, session) VALUES (2, 'agent', 'codex', 's');
             INSERT INTO history (seq, node_id, op, changed_at, changed_by, kind, title, body)
             VALUES (1, '01JABCDEFGHJKMNPQRSTVWXYZ0', 'create', 100, 1, 'note', 'A', 'one'),
                    (2, '01JABCDEFGHJKMNPQRSTVWXYZ0', 'update', 200, 2, 'note', 'A', 'two'),
                    (5, '01JABCDEFGHJKMNPQRSTVWXYZ0', 'delete', 300, 1, 'note', 'A', 'two');",
        )
        .unwrap();
        migrate(&mut conn).unwrap();
        assert_eq!(version(&conn).unwrap(), SCHEMA_VERSION);

        let changes: Vec<(i64, i64, i64, String, String)> = conn
            .prepare("SELECT id, changed_at, changed_by, action, subject FROM changes ORDER BY id")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let node = "01JABCDEFGHJKMNPQRSTVWXYZ0".to_string();
        assert_eq!(
            changes,
            [
                (1, 100, 1, "node.create".to_string(), node.clone()),
                (2, 200, 2, "node.update".to_string(), node.clone()),
                (5, 300, 1, "node.delete".to_string(), node),
            ]
        );
        let unlinked: i64 = conn
            .query_row(
                "SELECT count(*) FROM history WHERE change_id IS NOT seq OR restored_from IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unlinked, 0);
        // New changes are numbered after the migrated ones.
        conn.execute(
            "INSERT INTO changes (changed_at, changed_by, action) VALUES (400, 1, 'import')",
            [],
        )
        .unwrap();
        assert_eq!(conn.last_insert_rowid(), 6);
    }

    #[test]
    fn a_failed_migration_leaves_the_previous_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute_batch(MIGRATIONS[1]).unwrap();
        // A table that migration 3 creates makes it fail halfway.
        conn.execute_batch("CREATE TABLE link_history (x TEXT)")
            .unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        assert!(migrate(&mut conn).is_err());
        assert_eq!(version(&conn).unwrap(), 2);
        assert!(!table_exists(&conn, "changes"));
    }

    fn trigger_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type = 'trigger' AND name = ?1",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
            == 1
    }

    /// A version 3 database with one node, its history and a change.
    fn version_3_database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        for sql in &MIGRATIONS[..3] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 3).unwrap();
        conn.execute_batch(
            "INSERT INTO provenance (id, actor_kind, actor) VALUES (1, 'person', 'Ada');
             INSERT INTO nodes (id, kind, title, title_key, body, plain, created_at, updated_at,
                                created_by, updated_by)
             VALUES ('01JABCDEFGHJKMNPQRSTVWXYZ0', 'note', 'A', 'a', 'one', 'one', 1, 1, 1, 1);
             INSERT INTO changes (id, changed_at, changed_by, action, subject)
             VALUES (1, 1, 1, 'node.create', '01JABCDEFGHJKMNPQRSTVWXYZ0');
             INSERT INTO history (seq, node_id, op, changed_at, changed_by, kind, title, body,
                                  change_id)
             VALUES (1, '01JABCDEFGHJKMNPQRSTVWXYZ0', 'create', 1, 1, 'note', 'A', 'one', 1);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn migrates_a_version_3_database_adding_tables_and_changing_no_row() {
        let mut conn = version_3_database();
        let count = |conn: &Connection, table: &str| -> i64 {
            conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
        };
        let before: Vec<i64> = ["nodes", "history", "changes", "provenance"]
            .iter()
            .map(|t| count(&conn, t))
            .collect();
        migrate(&mut conn).unwrap();
        assert_eq!(version(&conn).unwrap(), 4);
        let after: Vec<i64> = ["nodes", "history", "changes", "provenance"]
            .iter()
            .map(|t| count(&conn, t))
            .collect();
        assert_eq!(before, after);
        for table in [
            "link_kinds",
            "views",
            "positions",
            "view_groups",
            "embedding_models",
            "embedding_chunks",
            "blobs",
            "attachments",
            "loro_docs",
            "loro_peers",
        ] {
            assert!(table_exists(&conn, table), "{table} is missing");
            assert_eq!(count(&conn, table), 0, "{table} is not empty");
        }
        assert!(trigger_exists(&conn, "embedding_chunks_stale"));
        assert!(trigger_exists(&conn, "loro_docs_body"));
    }

    #[test]
    fn embedding_chunks_go_stale_and_loro_documents_fall_behind() {
        let mut conn = version_3_database();
        migrate(&mut conn).unwrap();
        conn.execute_batch(
            "INSERT INTO embedding_models (id, name, dimensions, chunking, created_at)
             VALUES (1, 'BAAI/bge-small-en-v1.5', 384, 'knode-1', 1);
             INSERT INTO embedding_chunks (node_id, model_id, ord, text_start, text_end, embedded_at)
             VALUES ('01JABCDEFGHJKMNPQRSTVWXYZ0', 1, 0, 0, 3, 1);
             INSERT INTO loro_docs (node_id, version, updated_at)
             VALUES ('01JABCDEFGHJKMNPQRSTVWXYZ0', '{}', 1);",
        )
        .unwrap();
        let count = |conn: &Connection, table: &str| -> i64 {
            conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
        };
        // Changing neither the title nor the text keeps both.
        conn.execute("UPDATE nodes SET kind = 'idea'", []).unwrap();
        assert_eq!(count(&conn, "embedding_chunks"), 1);
        assert_eq!(count(&conn, "loro_docs"), 1);
        // A new body drops the chunks and marks the document as behind.
        conn.execute("UPDATE nodes SET body = 'two', plain = 'two'", [])
            .unwrap();
        assert_eq!(count(&conn, "embedding_chunks"), 0);
        assert_eq!(count(&conn, "loro_docs"), 0);
    }

    #[test]
    fn a_failed_migration_4_leaves_version_3() {
        let mut conn = version_3_database();
        conn.execute_batch("CREATE TABLE loro_peers (x TEXT)")
            .unwrap();
        assert!(migrate(&mut conn).is_err());
        assert_eq!(version(&conn).unwrap(), 3);
        assert!(!table_exists(&conn, "link_kinds"));
    }
}
