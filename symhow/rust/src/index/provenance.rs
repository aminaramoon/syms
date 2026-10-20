//! Persisted "how was this database produced" metadata, written to `meta` by
//! the index driver at the end of a full `symhow index` run. Mirrors
//! `symhow::index::Provenance`.

use rusqlite::Connection;

use crate::common::{Error, Result};
use crate::db::meta::{get_meta, set_meta};
use crate::db::WritableDb;
use crate::index::arg_sanitizer::SanitizerOptions;
use crate::index::compile_db::CompileDbOptions;
use crate::index::cuda_support::{cuda_mode_name, parse_cuda_mode};
use crate::index::file_registry::SystemFileRows;

// Bump kWalkerVersion whenever AstWalker's emitted facts change in a way that
// would make a database indexed by an older walker inconsistent with one
// indexed by a newer walker.
pub const K_PROVENANCE_VERSION: i32 = 1;
pub const K_WALKER_VERSION: i32 = 3;

// Single source of truth for the symhow_tu DDL.
pub const K_SYMHOW_TU_DDL: &str = "CREATE TABLE IF NOT EXISTS symhow_tu(path TEXT PRIMARY KEY, args_hash TEXT NOT NULL, file_node_id INTEGER, status TEXT NOT NULL DEFAULT 'ok', entry_count INTEGER NOT NULL DEFAULT 1);";

// Optional optimization state.
pub const K_SYMHOW_FILE_FINGERPRINT_DDL: &str = "CREATE TABLE IF NOT EXISTS symhow_file_fingerprint(file_node_id INTEGER PRIMARY KEY REFERENCES file(id) ON DELETE CASCADE, sha256 TEXT NOT NULL);";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub symhow_version: String,
    pub walker_version: i32,

    pub cdb_path: String, // absolute path to the compile_commands.json indexed

    pub db_options: CompileDbOptions,
    pub sanitizer_options: SanitizerOptions,
    pub system_file_rows: SystemFileRows,
    pub limit: i64, // -1 = no limit

    pub repo_root: String,   // absolute; empty if no git repo found
    pub repo_commit: String, // HEAD sha; empty if no git repo found
    pub dirty_paths: Vec<String>, // absolute paths, worktree-dirty at index time

    pub indexed_at_utc: String,    // ISO-8601 UTC timestamp of the full index
    pub last_update_at_utc: String, // ISO-8601 UTC timestamp of the most recent update

    pub notes_dir: String,
}

impl Default for Provenance {
    fn default() -> Self {
        Provenance {
            symhow_version: String::new(),
            walker_version: K_WALKER_VERSION,
            cdb_path: String::new(),
            db_options: CompileDbOptions::default(),
            sanitizer_options: SanitizerOptions::default(),
            system_file_rows: SystemFileRows::Direct,
            limit: -1,
            repo_root: String::new(),
            repo_commit: String::new(),
            dirty_paths: Vec::new(),
            indexed_at_utc: String::new(),
            last_update_at_utc: String::new(),
            notes_dir: String::new(),
        }
    }
}

fn options_to_json(prov: &Provenance) -> serde_json::Value {
    let remaps: Vec<serde_json::Value> = prov
        .db_options
        .remaps
        .iter()
        .map(|(from, to)| serde_json::json!({"from": from, "to": to}))
        .collect();

    let system_file_rows_name = match prov.system_file_rows {
        SystemFileRows::None => "none",
        SystemFileRows::Direct => "direct",
        SystemFileRows::All => "all",
    };

    serde_json::json!({
        "remaps": remaps,
        "include": prov.db_options.include_globs,
        "exclude": prov.db_options.exclude_globs,
        "system_file_rows": system_file_rows_name,
        "drop_args": prov.sanitizer_options.drop_arg_patterns,
        "extra_args": prov.sanitizer_options.extra_args,
        "resource_dir": prov.sanitizer_options.resource_dir,
        "gcc_toolchain": prov.sanitizer_options.gcc_toolchain,
        "std": prov.sanitizer_options.std_fallback,
        "c_std": prov.sanitizer_options.c_std_fallback,
        "cuda_path": prov.sanitizer_options.cuda_path,
        "cuda_mode": cuda_mode_name(prov.sanitizer_options.cuda_mode),
        "limit": prov.limit,
    })
}

fn string_array(j: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    j.get(key)?
        .as_array()?
        .iter()
        .map(|v| v.as_str().map(String::from))
        .collect()
}

fn options_from_json(j: &serde_json::Value, prov: &mut Provenance) {
    if let Some(remaps) = j.get("remaps").and_then(|v| v.as_array()) {
        for r in remaps {
            let from = r.get("from").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let to = r.get("to").and_then(|v| v.as_str()).unwrap_or("").to_string();
            prov.db_options.remaps.push((from, to));
        }
    }
    if let Some(v) = string_array(j, "include") {
        prov.db_options.include_globs = v;
    }
    if let Some(v) = string_array(j, "exclude") {
        prov.db_options.exclude_globs = v;
    }
    if let Some(v) = j.get("system_file_rows").and_then(|v| v.as_str()) {
        prov.system_file_rows = match v {
            "none" => SystemFileRows::None,
            "all" => SystemFileRows::All,
            _ => SystemFileRows::Direct,
        };
    }
    if let Some(v) = string_array(j, "drop_args") {
        prov.sanitizer_options.drop_arg_patterns = v;
    }
    if let Some(v) = string_array(j, "extra_args") {
        prov.sanitizer_options.extra_args = v;
    }
    if let Some(v) = j.get("resource_dir").and_then(|v| v.as_str()) {
        prov.sanitizer_options.resource_dir = v.to_string();
    }
    if let Some(v) = j.get("gcc_toolchain").and_then(|v| v.as_str()) {
        prov.sanitizer_options.gcc_toolchain = v.to_string();
    }
    if let Some(v) = j.get("std").and_then(|v| v.as_str()) {
        prov.sanitizer_options.std_fallback = v.to_string();
    }
    if let Some(v) = j.get("c_std").and_then(|v| v.as_str()) {
        prov.sanitizer_options.c_std_fallback = v.to_string();
    }
    if let Some(v) = j.get("cuda_path").and_then(|v| v.as_str()) {
        prov.sanitizer_options.cuda_path = v.to_string();
    }
    if let Some(v) = j.get("cuda_mode").and_then(|v| v.as_str()) {
        if let Ok(mode) = parse_cuda_mode(v) {
            prov.sanitizer_options.cuda_mode = mode;
        }
    }
    if let Some(v) = j.get("limit").and_then(|v| v.as_i64()) {
        prov.limit = v;
    }
}

/// Writes every Provenance field into `meta` inside the caller's already-open
/// transaction (does not itself begin/commit).
pub fn write_provenance(db: &WritableDb, prov: &Provenance) -> Result<()> {
    set_meta(db, "symhow_provenance_version", &K_PROVENANCE_VERSION.to_string())?;
    set_meta(db, "symhow_version", &prov.symhow_version)?;
    set_meta(db, "symhow_walker_version", &prov.walker_version.to_string())?;
    set_meta(db, "symhow_cdb_path", &prov.cdb_path)?;
    set_meta(db, "symhow_options_json", &options_to_json(prov).to_string())?;
    set_meta(db, "symhow_repo_root", &prov.repo_root)?;
    set_meta(db, "symhow_repo_commit", &prov.repo_commit)?;
    set_meta(
        db,
        "symhow_dirty_paths",
        &serde_json::to_string(&prov.dirty_paths).map_err(|e| Error::new(e.to_string()))?,
    )?;
    set_meta(db, "symhow_indexed_at", &prov.indexed_at_utc)?;
    set_meta(db, "symhow_last_update_at", &prov.last_update_at_utc)?;
    // Own meta key, NOT part of symhow_options_json. Only written when
    // non-empty.
    if !prov.notes_dir.is_empty() {
        set_meta(db, "symhow_notes_dir", &prov.notes_dir)?;
    }
    Ok(())
}

/// Reads Provenance back out of `meta`, given any open sqlite connection.
/// Returns None if 'symhow_provenance_version' isn't present.
pub fn read_provenance(conn: &Connection) -> Option<Provenance> {
    let version = get_meta(conn, "symhow_provenance_version");
    if version.is_none() {
        return None;
    }

    let mut prov = Provenance::default();
    prov.symhow_version = get_meta(conn, "symhow_version").unwrap_or_default();
    prov.walker_version = get_meta(conn, "symhow_walker_version")
        .unwrap_or_default()
        .parse::<i32>()
        .unwrap_or(0);
    prov.cdb_path = get_meta(conn, "symhow_cdb_path").unwrap_or_default();

    let options_json = get_meta(conn, "symhow_options_json").unwrap_or_default();
    if !options_json.is_empty() {
        if let Ok(j) = serde_json::from_str::<serde_json::Value>(&options_json) {
            options_from_json(&j, &mut prov);
        }
    }

    prov.repo_root = get_meta(conn, "symhow_repo_root").unwrap_or_default();
    prov.repo_commit = get_meta(conn, "symhow_repo_commit").unwrap_or_default();

    let dirty_json = get_meta(conn, "symhow_dirty_paths").unwrap_or_default();
    if !dirty_json.is_empty() {
        if let Ok(paths) = serde_json::from_str::<Vec<String>>(&dirty_json) {
            prov.dirty_paths = paths;
        }
    }

    prov.indexed_at_utc = get_meta(conn, "symhow_indexed_at").unwrap_or_default();
    prov.last_update_at_utc = get_meta(conn, "symhow_last_update_at").unwrap_or_default();
    prov.notes_dir = get_meta(conn, "symhow_notes_dir").unwrap_or_default();

    Some(prov)
}