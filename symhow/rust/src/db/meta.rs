//! `meta` table helpers. Mirrors `symhow::db::Meta`.

use rusqlite::Connection;

use crate::common::Result;

use super::writable_db::WritableDb;

/// Reads `meta`'s value for `key`. Returns `None` if there's no such row (or
/// `meta` doesn't exist, or the query fails — mirroring the C++ behavior of
/// swallowing prepare errors).
pub fn get_meta(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM meta WHERE key = ?",
        [key],
        |row| row.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

/// Idempotently sets `meta`'s value for `key` (DELETE then INSERT).
pub fn set_meta(db: &WritableDb, key: &str, value: &str) -> Result<()> {
    db.exec_bound("DELETE FROM meta WHERE key = ?", [key])?;
    db.exec_bound("INSERT INTO meta(key, value) VALUES(?, ?)", [key, value])
}

/// Deletes all `meta` rows for `key`, if any.
pub fn delete_meta(db: &WritableDb, key: &str) -> Result<()> {
    db.exec_bound("DELETE FROM meta WHERE key = ?", [key])
}