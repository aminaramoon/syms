//! SQLite persistence for files and symbols.
//! CONTRACT: see SPEC.md `store.rs`. Owner: storage agent.
//!
//! Layout (SPEC schema, verbatim):
//! - `meta(key, value)` — key/value metadata (`schema_version`, `head`, root).
//! - `files(id, path UNIQUE, lang, hash, size, indexed_at)`.
//! - `symbols(id, file_id → files(id) ON DELETE CASCADE, name, kind, container,
//!   start_line, start_col, end_line, end_col, signature)` plus indexes on
//!   `name` and `file_id`.
//!
//! Paths are repo-relative with `/` separators; langs and kinds are stored as
//! their canonical `Display` strings (`"rust"`, `"type-alias"`, …).

use crate::types::{FileRecord, Lang, SymbolRecord};
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Flattened symbol row handed to the search side (`fuzzy.rs` loads hits with
/// [`Store::all_symbols`] instead of poking at the schema):
/// `(path, lang, name, kind, container, start_line, start_col, end_line,
/// end_col, signature)`. Lines/cols keep the DB's i64 representation.
pub type SymbolRow = (
    String,
    Lang,
    String,
    String,
    Option<String>,
    i64,
    i64,
    i64,
    i64,
    Option<String>,
);

const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT
);
CREATE TABLE IF NOT EXISTS files (
    id INTEGER PRIMARY KEY,
    path TEXT UNIQUE NOT NULL,
    lang TEXT NOT NULL,
    hash TEXT NOT NULL,
    size INTEGER NOT NULL,
    indexed_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS symbols (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    container TEXT,
    start_line INTEGER NOT NULL,
    start_col INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    end_col INTEGER NOT NULL,
    signature TEXT
);
CREATE INDEX IF NOT EXISTS symbols_name ON symbols(name);
CREATE INDEX IF NOT EXISTS symbols_file ON symbols(file_id);
";

pub struct Store {
    conn: rusqlite::Connection,
}

impl Store {
    /// Open (or, with `create = true`, create) the index database.
    ///
    /// With `create = false` a missing file is an error pointing the user at
    /// `symwhere index`. The schema is created idempotently in both cases, and
    /// `schema_version = 1` is recorded only if absent.
    pub fn open(path: &Path, create: bool) -> Result<Store> {
        if !create && !path.exists() {
            anyhow::bail!(
                "no index database at {}; run `symwhere index` first",
                path.display()
            );
        }
        if create {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("create index directory {}", parent.display()))?;
                }
            }
        }
        let conn = rusqlite::Connection::open(path)
            .with_context(|| format!("open sqlite database {}", path.display()))?;
        finish_open(conn)
    }

    /// In-memory store, used by the unit tests (and handy for experiments).
    #[allow(dead_code)] // test-only helper; referenced from #[cfg(test)] code
    pub fn open_in_memory() -> Result<Store> {
        let conn = rusqlite::Connection::open_in_memory().context("open in-memory database")?;
        finish_open(conn)
    }

    /// Insert or update a file row and return its id.
    ///
    /// The path is the unique key: re-indexing an unchanged path keeps its id
    /// and bumps `lang`/`hash`/`size`/`indexed_at`.
    /// Granular variant kept for tests and future partial-update paths; the
    /// indexer uses the atomic [`Self::upsert_file_with_symbols`].
    #[allow(dead_code)]
    pub fn upsert_file(&mut self, rec: &FileRecord) -> Result<i64> {
        self.conn
            .execute(
                "INSERT INTO files(path, lang, hash, size, indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(path) DO UPDATE SET
                     lang = excluded.lang,
                     hash = excluded.hash,
                     size = excluded.size,
                     indexed_at = excluded.indexed_at",
                params![
                    rec.path,
                    rec.lang.name(),
                    rec.hash,
                    rec.size as i64,
                    now_unix()
                ],
            )
            .with_context(|| format!("upsert file row for {}", rec.path))?;
        let id: i64 = self
            .conn
            .query_row(
                "SELECT id FROM files WHERE path = ?1",
                params![rec.path],
                |row| row.get(0),
            )
            .with_context(|| format!("fetch file id for {}", rec.path))?;
        Ok(id)
    }

    /// Atomically upsert a file row and replace its whole symbol set in a
    /// single transaction, so a crash can never leave a fresh content hash
    /// paired with stale or missing symbols (which a later hash-scan update
    /// would then never repair).
    pub fn upsert_file_with_symbols(
        &mut self,
        rec: &FileRecord,
        syms: &[SymbolRecord],
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction()
            .context("begin file+symbols transaction")?;
        tx.execute(
            "INSERT INTO files(path, lang, hash, size, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET
                 lang = excluded.lang,
                 hash = excluded.hash,
                 size = excluded.size,
                 indexed_at = excluded.indexed_at",
            params![
                rec.path,
                rec.lang.name(),
                rec.hash,
                rec.size as i64,
                now_unix()
            ],
        )
        .with_context(|| format!("upsert file row for {}", rec.path))?;
        let file_id: i64 = tx
            .query_row(
                "SELECT id FROM files WHERE path = ?1",
                params![rec.path],
                |row| row.get(0),
            )
            .with_context(|| format!("fetch file id for {}", rec.path))?;
        tx.execute("DELETE FROM symbols WHERE file_id = ?1", params![file_id])
            .context("clear old symbols")?;
        Self::insert_symbols(&tx, file_id, syms)?;
        tx.commit().context("commit file+symbols")?;
        Ok(())
    }

    /// Atomically replace the symbol set of one file: delete its rows, then
    /// bulk-insert the new ones inside a single transaction.
    #[allow(dead_code)]
    pub fn replace_symbols(&mut self, file_id: i64, syms: &[SymbolRecord]) -> Result<()> {
        let tx = self
            .conn
            .transaction()
            .context("begin replace_symbols transaction")?;
        tx.execute("DELETE FROM symbols WHERE file_id = ?1", params![file_id])
            .context("clear old symbols")?;
        Self::insert_symbols(&tx, file_id, syms)?;
        tx.commit().context("commit replace_symbols")?;
        Ok(())
    }

    /// Bulk-insert one file's symbol rows (shared by the transactional writers).
    fn insert_symbols(
        tx: &rusqlite::Transaction<'_>,
        file_id: i64,
        syms: &[SymbolRecord],
    ) -> Result<()> {
        let mut stmt = tx
            .prepare(
                "INSERT INTO symbols(file_id, name, kind, container,
                                start_line, start_col, end_line, end_col, signature)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )
            .context("prepare symbol insert")?;
        for sym in syms {
            stmt.execute(params![
                file_id,
                sym.name,
                sym.kind.name(),
                sym.container,
                sym.start_line as i64,
                sym.start_col as i64,
                sym.end_line as i64,
                sym.end_col as i64,
                sym.signature,
            ])
            .with_context(|| format!("insert symbol `{}`", sym.name))?;
        }
        Ok(())
    }

    /// Drop a file row; the `ON DELETE CASCADE` foreign key clears its symbols
    /// (requires the `foreign_keys = ON` pragma, set at open time).
    /// Removing an unknown path is a no-op.
    pub fn remove_file(&mut self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM files WHERE path = ?1", params![path])
            .with_context(|| format!("remove file row for {path}"))?;
        Ok(())
    }

    /// `path → blake3 hash` for every indexed file.
    pub fn file_hashes(&self) -> Result<HashMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, hash FROM files")
            .context("prepare file_hashes query")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .context("query file hashes")?;
        let mut map = HashMap::new();
        for row in rows {
            let (path, hash) = row.context("read file hash row")?;
            map.insert(path, hash);
        }
        Ok(map)
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let value = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .with_context(|| format!("read meta key `{key}`"))?;
        Ok(value)
    }

    pub fn set_meta(&mut self, key: &str, value: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO meta(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .with_context(|| format!("write meta key `{key}`"))?;
        Ok(())
    }

    /// Total number of symbol rows in the index.
    pub fn count_symbols(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
            .context("count symbols")?;
        Ok(n.max(0) as u64)
    }

    /// Number of symbol rows belonging to one file (by path); 0 for unknown
    /// paths. Used by `update --dry-run` to report per-file symbol deltas.
    pub fn count_symbols_for_file(&self, path: &str) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM symbols s JOIN files f ON s.file_id = f.id
                 WHERE f.path = ?1",
                params![path],
                |row| row.get(0),
            )
            .with_context(|| format!("count symbols for {path}"))?;
        Ok(n.max(0) as u64)
    }

    /// Every symbol row joined with its file, for the search side (fuzzy.rs):
    /// `SELECT f.path, f.lang, s.name, s.kind, ... FROM symbols s
    ///  JOIN files f ON s.file_id = f.id`.
    /// Kinds stay raw strings so the caller can filter/parse them itself.
    #[allow(dead_code)] // consumed by fuzzy.rs (search agent)
    pub fn all_symbols(&self) -> Result<Vec<SymbolRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT f.path, f.lang, s.name, s.kind, s.container,
                        s.start_line, s.start_col, s.end_line, s.end_col, s.signature
                 FROM symbols s JOIN files f ON s.file_id = f.id
                 ORDER BY f.path, s.name",
            )
            .context("prepare all_symbols query")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            })
            .context("query all symbols")?;
        let mut out = Vec::new();
        for row in rows {
            let (path, lang, name, kind, container, sl, sc, el, ec, sig) =
                row.context("read symbol row")?;
            let lang = Lang::from_str(&lang)
                .with_context(|| format!("corrupt lang `{lang}` for {path} in index"))?;
            out.push((path, lang, name, kind, container, sl, sc, el, ec, sig));
        }
        Ok(out)
    }
}

/// Shared open tail: pragmas + idempotent schema + schema_version stamp.
fn finish_open(conn: rusqlite::Connection) -> Result<Store> {
    // journal_mode returns a row, so go through execute_batch rather than
    // pragma_update; the values are fixed literals.
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;",
    )
    .context("set sqlite pragmas")?;
    conn.execute_batch(SCHEMA).context("create schema")?;
    conn.execute(
        "INSERT INTO meta(key, value) SELECT 'schema_version', '1'
         WHERE NOT EXISTS (SELECT 1 FROM meta WHERE key = 'schema_version')",
        [],
    )
    .context("stamp schema_version")?;
    Ok(Store { conn })
}

/// Resolve the database location:
/// - `--db` pointing at an existing directory or a path with a trailing `/`
///   → `<dir>/index.db`;
/// - `--db` pointing elsewhere → used verbatim as the file path;
/// - no `--db` → `<root>/.symwhere/index.db`.
pub fn db_path(root: &Path, cli_db: &Option<PathBuf>) -> PathBuf {
    match cli_db {
        Some(db) => {
            let looks_like_dir = db.is_dir() || db.to_string_lossy().ends_with('/');
            if looks_like_dir {
                db.join("index.db")
            } else {
                db.clone()
            }
        }
        None => root.join(".symwhere").join("index.db"),
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SymbolKind;

    fn file(path: &str, lang: Lang, hash: &str) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            lang,
            hash: hash.to_string(),
            size: 42,
        }
    }

    fn sym(name: &str, kind: SymbolKind, container: Option<&str>) -> SymbolRecord {
        SymbolRecord {
            name: name.to_string(),
            kind,
            container: container.map(str::to_string),
            start_line: 3,
            start_col: 4,
            end_line: 9,
            end_col: 10,
            signature: Some(format!("fn {name}()")),
        }
    }

    #[test]
    fn db_path_defaults_and_overrides() {
        let root = Path::new("/repo");
        assert_eq!(
            db_path(root, &None),
            PathBuf::from("/repo/.symwhere/index.db")
        );
        // Plain non-existent path → used as a file path.
        let file_db = Some(PathBuf::from("/tmp/custom.db"));
        assert_eq!(db_path(root, &file_db), PathBuf::from("/tmp/custom.db"));
        // Trailing slash → directory semantics even if it doesn't exist yet.
        let trailing = Some(PathBuf::from("/tmp/idxdir/"));
        assert_eq!(
            db_path(root, &trailing),
            PathBuf::from("/tmp/idxdir/index.db")
        );
    }

    #[test]
    fn db_path_existing_dir_gets_index_db() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("symwhere-dbpath-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            db_path(Path::new("/repo"), &Some(dir.clone())),
            dir.join("index.db")
        );
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn open_missing_db_without_create_is_an_error() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let missing =
            std::env::temp_dir().join(format!("symwhere-missing-{}-{nanos}", std::process::id()));
        let err = match Store::open(&missing, false) {
            Ok(_) => panic!("opening a missing db without create must fail"),
            Err(err) => err,
        };
        let msg = format!("{err:#}");
        assert!(msg.contains("no index database at"), "got: {msg}");
        assert!(msg.contains("symwhere index"), "got: {msg}");
    }

    #[test]
    fn open_create_makes_parent_dirs() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let db = std::env::temp_dir()
            .join(format!("symwhere-create-{}-{nanos}", std::process::id()))
            .join("nested")
            .join("index.db");
        let _store = Store::open(&db, true).unwrap();
        assert!(db.exists());
        std::fs::remove_dir_all(db.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn upsert_replace_and_cascade() {
        let mut store = Store::open_in_memory().unwrap();

        let id1 = store
            .upsert_file(&file("src/a.rs", Lang::Rust, "h1"))
            .unwrap();
        let id2 = store
            .upsert_file(&file("src/b.rs", Lang::Cpp, "h2"))
            .unwrap();
        assert_ne!(id1, id2);

        // Re-upserting the same path keeps the id and refreshes the row.
        let id1b = store
            .upsert_file(&file("src/a.rs", Lang::Rust, "h1b"))
            .unwrap();
        assert_eq!(id1, id1b);
        let hashes = store.file_hashes().unwrap();
        assert_eq!(hashes.len(), 2);
        assert_eq!(hashes.get("src/a.rs").unwrap(), "h1b");

        store
            .replace_symbols(
                id1,
                &[
                    sym("alpha", SymbolKind::Function, None),
                    sym("beta", SymbolKind::Function, Some("ns")),
                ],
            )
            .unwrap();
        store
            .replace_symbols(id2, &[sym("Klass", SymbolKind::Class, None)])
            .unwrap();
        assert_eq!(store.count_symbols().unwrap(), 3);
        assert_eq!(store.count_symbols_for_file("src/a.rs").unwrap(), 2);

        // Replacing wipes the previous set instead of appending.
        store
            .replace_symbols(id1, &[sym("alpha2", SymbolKind::Function, None)])
            .unwrap();
        assert_eq!(store.count_symbols_for_file("src/a.rs").unwrap(), 1);

        // Removing a file cascades to its symbols.
        store.remove_file("src/a.rs").unwrap();
        let hashes = store.file_hashes().unwrap();
        assert!(hashes.contains_key("src/b.rs"));
        assert!(!hashes.contains_key("src/a.rs"));
        assert_eq!(store.count_symbols().unwrap(), 1);

        // Removing an unknown path is a no-op.
        store.remove_file("nope.rs").unwrap();
    }

    #[test]
    fn meta_roundtrip() {
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(store.get_meta("head").unwrap(), None);
        store.set_meta("head", "abc123").unwrap();
        assert_eq!(store.get_meta("head").unwrap().as_deref(), Some("abc123"));
        store.set_meta("head", "def456").unwrap();
        assert_eq!(store.get_meta("head").unwrap().as_deref(), Some("def456"));
    }

    #[test]
    fn schema_version_stamped_once() {
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(
            store.get_meta("schema_version").unwrap().as_deref(),
            Some("1")
        );
        // A second open (same semantics here) must not duplicate it.
        store.set_meta("schema_version", "1").unwrap();
        assert_eq!(
            store.get_meta("schema_version").unwrap().as_deref(),
            Some("1")
        );
    }

    #[test]
    fn all_symbols_joins_files() {
        let mut store = Store::open_in_memory().unwrap();
        let id = store
            .upsert_file(&file("src/a.rs", Lang::Rust, "h"))
            .unwrap();
        store
            .replace_symbols(id, &[sym("alpha", SymbolKind::Function, Some("ns::Outer"))])
            .unwrap();

        let rows = store.all_symbols().unwrap();
        assert_eq!(rows.len(), 1);
        let (path, lang, name, kind, container, sl, sc, el, ec, sig) = &rows[0];
        assert_eq!(path, "src/a.rs");
        assert_eq!(*lang, Lang::Rust);
        assert_eq!(name, "alpha");
        assert_eq!(kind, "function");
        assert_eq!(container.as_deref(), Some("ns::Outer"));
        assert_eq!((*sl, *sc, *el, *ec), (3, 4, 9, 10));
        assert_eq!(sig.as_deref(), Some("fn alpha()"));
    }
}
