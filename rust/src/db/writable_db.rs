//! RAII wrapper around a READ-WRITE sqlite3 handle. Mirrors
//! `symhow::db::WritableDb`.

use rusqlite::{Connection, OpenFlags};

use crate::common::{Error, Result};

pub struct WritableDb {
    path: String,
    conn: Connection,
    in_transaction: bool,
}

impl WritableDb {
    /// Opens `path` READ-WRITE (the file must already exist). If
    /// `foreign_keys` is true, immediately executes `PRAGMA foreign_keys=ON;`.
    pub fn open(path: impl Into<String>, foreign_keys: bool) -> Result<WritableDb> {
        let path = path.into();
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|e| Error::new(format!("failed to open '{}' (read-write): {}", path, e)))?;

        let db = WritableDb { path, conn, in_transaction: false };
        if foreign_keys {
            db.exec("PRAGMA foreign_keys=ON;")?;
        }
        Ok(db)
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Executes `sql` directly (may contain multiple statements).
    pub fn exec(&self, sql: &str) -> Result<()> {
        self.conn
            .execute_batch(sql)
            .map_err(|e| Error::new(format!("sqlite error running '{}': {}", sql, e)))
    }

    /// Prepares (and caches) `sql`, binds `params`, and steps to completion.
    /// For INSERT/UPDATE/DELETE where no result rows are expected.
    pub fn exec_bound<P: rusqlite::Params>(&self, sql: &str, params: P) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare_cached(sql)
            .map_err(|e| Error::new(format!("failed to prepare statement '{}': {}", sql, e)))?;
        stmt.execute(params)
            .map_err(|e| Error::new(format!("sqlite error running '{}': {}", sql, e)))?;
        Ok(())
    }

    /// Prepares (and caches) `sql` (no parameters) and invokes `on_row` once
    /// per result row.
    pub fn query<F>(&self, sql: &str, on_row: F) -> Result<()>
    where
        F: FnMut(&rusqlite::Row<'_>) -> Result<()>,
    {
        self.query_bound(sql, [], on_row)
    }

    /// Prepares (and caches) `sql`, binds `params`, and invokes `on_row` once
    /// per result row.
    pub fn query_bound<P, F>(&self, sql: &str, params: P, mut on_row: F) -> Result<()>
    where
        P: rusqlite::Params,
        F: FnMut(&rusqlite::Row<'_>) -> Result<()>,
    {
        let mut stmt = self
            .conn
            .prepare_cached(sql)
            .map_err(|e| Error::new(format!("failed to prepare statement '{}': {}", sql, e)))?;
        let mut rows = stmt
            .query(params)
            .map_err(|e| Error::new(format!("sqlite error running '{}': {}", sql, e)))?;
        while let Some(row) = rows
            .next()
            .map_err(|e| Error::new(format!("sqlite error running '{}': {}", sql, e)))?
        {
            on_row(&row)?;
        }
        Ok(())
    }

    pub fn changes(&self) -> i64 {
        self.conn.changes() as i64
    }

    pub fn begin(&mut self) -> Result<()> {
        self.exec("BEGIN;")?;
        self.in_transaction = true;
        Ok(())
    }

    pub fn commit(&mut self) -> Result<()> {
        self.exec("COMMIT;")?;
        self.in_transaction = false;
        Ok(())
    }

    pub fn rollback(&mut self) -> Result<()> {
        self.exec("ROLLBACK;")?;
        self.in_transaction = false;
        Ok(())
    }

    pub fn table_exists(&self, name: &str) -> bool {
        let mut found = false;
        let _ = self.query_bound(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?",
            [name],
            |_row| {
                found = true;
                Ok(())
            },
        );
        found
    }
}

impl Drop for WritableDb {
    fn drop(&mut self) {
        if self.in_transaction {
            let _ = self.conn.execute_batch("ROLLBACK;");
        }
    }
}