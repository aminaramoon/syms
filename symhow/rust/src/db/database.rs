//! Read-only SQLite access to a Sourcetrail-schema (storage_version 25)
//! database. Mirrors `symhow::db::Database`.

use std::sync::OnceLock;

use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags};

use crate::common::{Error, Result};
use crate::common::log::log_info;

use super::writable_db::WritableDb;

/// Pins the schema version this whole query engine was written against.
pub const K_EXPECTED_STORAGE_VERSION: i32 = 25;

type IndexDef = (&'static str, &'static str);

/// Exactly the 9 statements `symhow index` runs post-write.
fn index_definitions() -> &'static [IndexDef] {
    static DEFS: OnceLock<Vec<IndexDef>> = OnceLock::new();
    DEFS.get_or_init(|| {
        vec![
            ("symhow_edge_source_idx", "CREATE INDEX IF NOT EXISTS symhow_edge_source_idx ON edge(source_node_id);"),
            ("symhow_edge_target_idx", "CREATE INDEX IF NOT EXISTS symhow_edge_target_idx ON edge(target_node_id);"),
            ("symhow_edge_type_idx", "CREATE INDEX IF NOT EXISTS symhow_edge_type_idx ON edge(type);"),
            ("symhow_node_type_idx", "CREATE INDEX IF NOT EXISTS symhow_node_type_idx ON node(type);"),
            ("symhow_srcloc_file_idx", "CREATE INDEX IF NOT EXISTS symhow_srcloc_file_idx ON source_location(file_node_id);"),
            ("symhow_occurrence_elem_idx", "CREATE INDEX IF NOT EXISTS symhow_occurrence_elem_idx ON occurrence(element_id);"),
            ("symhow_occurrence_loc_idx", "CREATE INDEX IF NOT EXISTS symhow_occurrence_loc_idx ON occurrence(source_location_id);"),
            ("symhow_file_path_idx", "CREATE INDEX IF NOT EXISTS symhow_file_path_idx ON file(path);"),
            ("symhow_symbol_defkind_idx", "CREATE INDEX IF NOT EXISTS symhow_symbol_defkind_idx ON symbol(definition_kind);"),
        ]
    })
}

/// Names of the 9 indices `ensure_indices()` creates.
pub fn expected_index_names() -> Vec<String> {
    index_definitions().iter().map(|(name, _)| name.to_string()).collect()
}

/// Opens `db_path` READ-WRITE and runs the 9 `CREATE INDEX IF NOT EXISTS`
/// statements, then closes it again.
pub fn ensure_indices(db_path: &str) -> Result<()> {
    let mut db = WritableDb::open(db_path, false)
        .map_err(|e| Error::new(format!("symhow query --ensure-indices: {}", e.0)))?;
    db.begin()?;
    for (_, stmt) in index_definitions() {
        db.exec(stmt)?;
    }
    db.commit()
}

/// Which of `expected_index_names()` are NOT present in `db`'s sqlite_master.
pub fn missing_index_names(db: &Database) -> Vec<String> {
    let mut present = Vec::new();
    let _ = db.run("SELECT name FROM sqlite_master WHERE type = 'index'", [], |row| {
        present.push(row.get::<_, String>(0).unwrap_or_default());
        Ok(())
    });

    expected_index_names()
        .into_iter()
        .filter(|expected| !present.contains(expected))
        .collect()
}

pub struct Database {
    path: String,
    conn: Connection,
    storage_version: i32,
}

impl Database {
    /// Opens `path` read-only. Errors if the file can't be opened, doesn't
    /// have a `meta` table, has no `storage_version` row, or that row's value
    /// isn't exactly "25".
    pub fn open(path: impl Into<String>, verbosity: i32) -> Result<Database> {
        let path = path.into();
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| Error::new(format!("failed to open '{}' (read-only): {}", path, e)))?;

        let version_text: Option<String> = match conn.query_row(
            "SELECT value FROM meta WHERE key = 'storage_version'",
            [],
            |row| row.get(0),
        ) {
            Ok(v) => Some(v),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => {
                return Err(Error::new(format!(
                    "'{}' does not look like a Sourcetrail-schema database (no readable `meta` table): {}",
                    path, e
                )))
            }
        };

        let Some(version_text) = version_text else {
            return Err(Error::new(format!(
                "'{}' has no `meta.storage_version` row -- not a valid Sourcetrail-schema database",
                path
            )));
        };

        let storage_version: i32 = version_text.parse().unwrap_or(-1);
        if storage_version != K_EXPECTED_STORAGE_VERSION {
            return Err(Error::new(format!(
                "'{}' has storage_version '{}', but symhow query only supports {} (no migration support -- re-index with this build's `symhow index`)",
                path, version_text, K_EXPECTED_STORAGE_VERSION
            )));
        }

        let db = Database { path, conn, storage_version };

        if verbosity > 0 {
            for name in missing_index_names(&db) {
                log_info(&format!(
                    "symhow query: warning: expected index '{}' is missing (run with --ensure-indices to create it)",
                    name
                ));
            }
        }

        Ok(db)
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn storage_version(&self) -> i32 {
        self.storage_version
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Runs `sql` with `params`, invoking `on_row` once per result row.
    pub fn run<P, F>(&self, sql: &str, params: P, mut on_row: F) -> Result<()>
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

    /// Chunked `WHERE col IN (?,?,...)` helper. `sql_template` must contain
    /// the literal marker "{IN}" exactly once, replaced with a comma-joined
    /// "?,?,..." placeholder list sized to each chunk of `ids`. `extra` values
    /// are appended after the chunk's ids (for trailing `AND (...)` clauses).
    /// No-op if `ids` is empty.
    pub fn run_chunked_in<F>(
        &self,
        sql_template: &str,
        ids: &[i64],
        extra: &[Value],
        on_row: F,
        chunk_size: usize,
    ) -> Result<()>
    where
        F: FnMut(&rusqlite::Row<'_>) -> Result<()>,
    {
        if ids.is_empty() {
            return Ok(());
        }
        let mut on_row = on_row;
        for chunk in ids.chunks(chunk_size) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = sql_template.replace("{IN}", &placeholders);
            let mut params: Vec<Value> = chunk.iter().map(|&id| Value::Integer(id)).collect();
            params.extend(extra.iter().cloned());
            self.run(&sql, rusqlite::params_from_iter(params), &mut on_row)?;
        }
        Ok(())
    }
}