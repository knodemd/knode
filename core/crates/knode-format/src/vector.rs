//! Loading sqlite-vec, the SQLite extension that holds the vectors of embeddings (19.4).

use rusqlite::Connection;

/// Loads sqlite-vec into the connection and returns whether it is there. sqlite-vec is compiled
/// into the program (the `vector-search` feature), so nothing is read from disk.
#[cfg(feature = "vector-search")]
pub(crate) fn load(conn: &Connection) -> bool {
    use std::ffi::{c_char, c_int, c_void};

    use rusqlite::ffi;

    /// The entry point of a SQLite extension.
    type Init = unsafe extern "C" fn(*mut ffi::sqlite3, *mut *mut c_char, *const c_void) -> c_int;

    // SAFETY: `sqlite3_vec_init` is the extension's entry point, declared without arguments by
    // the sqlite-vec crate; its C definition takes the connection, a pointer for an error
    // message and the API routines, which are unused when it is compiled into the program
    // (SQLITE_CORE). The connection handle is valid for the duration of the call, and an error
    // message, if any, is freed with sqlite3_free as SQLite requires.
    unsafe {
        let init: Init = std::mem::transmute(sqlite_vec::sqlite3_vec_init as *const ());
        let mut message: *mut c_char = std::ptr::null_mut();
        let code = init(conn.handle(), &mut message, std::ptr::null());
        if !message.is_null() {
            ffi::sqlite3_free(message.cast());
        }
        code == ffi::SQLITE_OK
    }
}

/// Without the `vector-search` feature there is no sqlite-vec.
#[cfg(not(feature = "vector-search"))]
pub(crate) fn load(_conn: &Connection) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "vector-search")]
    fn loads_sqlite_vec_into_a_connection() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(load(&conn));
        let version: String = conn
            .query_row("SELECT vec_version()", [], |row| row.get(0))
            .unwrap();
        assert!(version.starts_with("v0.1."), "{version}");
        conn.execute_batch(
            "CREATE VIRTUAL TABLE v USING vec0 (embedding float[2] distance_metric=cosine);
             INSERT INTO v (rowid, embedding) VALUES (1, '[1, 0]'), (2, '[0, 1]');",
        )
        .unwrap();
        let nearest: i64 = conn
            .query_row(
                "SELECT rowid FROM v WHERE embedding MATCH '[0.9, 0.1]' AND k = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(nearest, 1);
    }

    #[test]
    #[cfg(not(feature = "vector-search"))]
    fn without_the_feature_nothing_is_loaded() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(!load(&conn));
    }
}
