//! Persistent relocation of a portable database. Source rows stay relative;
//! only root/provenance metadata changes, atomically.
use crate::common::paths::{encode_source_path, lexical_normalize};
use crate::common::{Error, Result};
use crate::db::meta::{get_meta, set_meta};
use crate::db::{Database, WritableDb};

fn relocate(path: &str, old: &str, new: &str) -> String {
    if path.is_empty() || !std::path::Path::new(path).is_absolute() {
        return path.to_string();
    }
    let relative = encode_source_path(path, old);
    if std::path::Path::new(&relative).is_absolute() {
        path.to_string()
    } else {
        lexical_normalize(&format!("{}/{}", new, relative))
    }
}

fn validate(conn: &rusqlite::Connection) -> Result<String> {
    if get_meta(conn, "storage_version").as_deref() != Some("25") {
        return Err(Error::new("unsupported storage version -- re-index with this build"));
    }
    if get_meta(conn, "symhow_update_in_progress").as_deref() == Some("1") {
        return Err(Error::new("update in progress -- finish/recover the update before --rebind"));
    }
    if get_meta(conn, "symhow_path_format").as_deref() != Some("relative-v1") {
        return Err(Error::new("legacy/unsupported path format -- re-index from scratch before --rebind (root-derived symbol hashes cannot be safely migrated)"));
    }
    let old = get_meta(conn, "symhow_source_root").unwrap_or_default();
    if old.is_empty() || !std::path::Path::new(&old).is_absolute() {
        return Err(Error::new("missing absolute source root -- re-index before --rebind"));
    }
    Ok(lexical_normalize(&old))
}

struct RebindLock(String);
impl Drop for RebindLock {
    fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); }
}

pub fn rebind_database(db_path: &str, new_root: &str) -> Result<()> {
    if new_root.is_empty() || !std::path::Path::new(new_root).is_dir() {
        return Err(Error::new("--rebind root must be an existing directory"));
    }
    let new = crate::common::paths::normalize(new_root);
    // Check before opening read-write: invalid requests must not mutate the DB.
    for suffix in [".symhow-lock", "-journal", "-wal"] {
        if std::path::Path::new(&format!("{}{}", db_path, suffix)).exists() {
            return Err(Error::new("database has an active update lock/journal/WAL -- finish it before --rebind"));
        }
    }
    {
        let reader = Database::open(db_path, 0)?;
        validate(reader.conn())?;
    }
    let lock_path = format!("{}.symhow-lock", db_path);
    std::fs::OpenOptions::new().write(true).create_new(true).open(&lock_path)
        .map_err(|e| Error::new(format!("cannot acquire rebind lock '{}': {}", lock_path, e)))?;
    let _lock = RebindLock(lock_path);
    let mut db = WritableDb::open(db_path, false)?;
    db.begin()?;
    let result = (|| -> Result<()> {
        let old = validate(db.conn())?;
        if old == new {
            return Ok(());
        }
        for key in ["symhow_repo_root", "symhow_cdb_path", "symhow_notes_dir"] {
            if let Some(value) = get_meta(db.conn(), key) {
                set_meta(&db, key, &relocate(&value, &old, &new))?;
            }
        }
        if let Some(raw) = get_meta(db.conn(), "symhow_dirty_paths") {
            let mut paths: Vec<String> = serde_json::from_str(&raw)
                .map_err(|e| Error::new(format!("invalid dirty paths metadata: {}", e)))?;
            for path in &mut paths { *path = relocate(path, &old, &new); }
            set_meta(&db, "symhow_dirty_paths", &serde_json::to_string(&paths).unwrap())?;
        }
        let raw = get_meta(db.conn(), "symhow_options_json").unwrap_or_else(|| "{}".into());
        let mut options: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| Error::new(format!("invalid compiler options metadata: {}", e)))?;
        if !options.is_object() { return Err(Error::new("invalid compiler options metadata")); }
        for key in ["resource_dir", "gcc_toolchain", "cuda_path"] {
            if let Some(value) = options.get(key).and_then(|v| v.as_str()) {
                options[key] = relocate(value, &old, &new).into();
            }
        }
        for key in ["include", "exclude", "extra_args", "drop_args"] {
            if let Some(values) = options.get_mut(key).and_then(|v| v.as_array_mut()) {
                for value in values {
                    if let Some(token) = value.as_str() {
                        *value = crate::index::compile_db::relocate_root_token(
                            token, &new, &[(old.clone(), new.clone())]).into();
                    }
                }
            }
        }
        set_meta(&db, "symhow_options_json", &options.to_string())?;
        // Root relocations are typed separately from user substring remaps.
        let raw_roots = get_meta(db.conn(), "symhow_root_remaps").unwrap_or_else(|| "[]".into());
        let mut root_remaps: Vec<serde_json::Value> = serde_json::from_str(&raw_roots)
            .map_err(|e| Error::new(format!("invalid root remaps metadata: {}", e)))?;
        for remap in &mut root_remaps {
            let to = remap.get("to").and_then(|v| v.as_str())
                .ok_or_else(|| Error::new("invalid root remap destination"))?;
            if remap.get("from").and_then(|v| v.as_str()).is_none() {
                return Err(Error::new("invalid root remap source"));
            }
            remap["to"] = relocate(to, &old, &new).into();
        }
        root_remaps.retain(|remap| remap["from"].as_str() != Some(new.as_str()));
        root_remaps.push(serde_json::json!({"from": old, "to": new}));
        set_meta(&db, "symhow_root_remaps", &serde_json::to_string(&root_remaps).unwrap())?;
        set_meta(&db, "symhow_source_root", &new)?;
        Ok(())
    })();
    match result {
        Ok(()) => db.commit(),
        Err(e) => { let _ = db.rollback(); Err(e) }
    }
}

/// Perform and strip --rebind before existing command parsers see argv.
/// Standalone invocation returns success without dispatching a command.
pub(super) fn preprocess(args: &[String]) -> std::result::Result<Option<Vec<String>>, i32> {
    let Some(pos) = args.iter().position(|arg| arg == "--rebind") else {
        return Ok(Some(args.to_vec()));
    };
    if pos + 1 >= args.len() || args[pos + 1].is_empty() || args[pos + 1].starts_with('-') {
        eprintln!("symhow: --rebind requires a directory");
        return Err(1);
    }
    let mut stripped = args.to_vec();
    let root = stripped.remove(pos + 1);
    stripped.remove(pos);
    if stripped.iter().any(|arg| arg == "--rebind") {
        eprintln!("symhow: --rebind may only be supplied once");
        return Err(1);
    }
    // Help/version must never perform a persistent mutation.
    if stripped.iter().any(|arg| matches!(arg.as_str(), "--help" | "-h" | "--version")) {
        return Ok(Some(stripped));
    }
    let db = stripped.windows(2).rfind(|pair| pair[0] == "-d" || pair[0] == "--db")
        .map(|pair| pair[1].clone())
        .unwrap_or_else(|| std::env::var("SYMHOW_DB").unwrap_or_else(|_| "./symhow.srctrldb".into()));
    let standalone = stripped.get(1).map_or(true, |arg| arg.starts_with('-'));
    if !standalone && !matches!(stripped[1].as_str(), "query" | "update" | "notes" | "graph") {
        eprintln!("symhow: --rebind requires an existing database command");
        return Err(1);
    }
    if !standalone {
        let verb = stripped.get(2).map(String::as_str).unwrap_or("");
        let valid = match stripped[1].as_str() {
            "query" => matches!(verb, "callers" | "callees" | "bases" | "derived"
                | "overrides" | "overridden-by" | "members" | "neighbors"
                | "references" | "definition" | "search" | "file" | "stats" | "context"),
            "notes" => matches!(verb, "sync" | "show" | "status" | "symbols" | "scaffold" | "accept"),
            "graph" => matches!(verb, "centrality" | "pagerank" | "dominators" | "slice" | "path"),
            "update" => true,
            _ => false,
        };
        if !valid {
            eprintln!("symhow: --rebind requires a recognized database-backed command");
            return Err(1);
        }
    }
    if standalone {
        let rest = &stripped[1..];
        if !rest.is_empty() && !(rest.len() == 2 && (rest[0] == "-d" || rest[0] == "--db")) {
            eprintln!("symhow: standalone --rebind accepts only -d/--db DB");
            return Err(1);
        }
    }
    // Reject malformed query/update invocations before the persistent rebind.
    // Reuse their parsers without opening a database or running either command.
    let validation_code = match stripped.get(1).map(String::as_str) {
        Some("query") => super::query_cmd::validate_query_cmd(&stripped, 2),
        Some("update") => super::update_cmd::validate_update_cmd(&stripped, 2),
        _ => 0,
    };
    if validation_code != 0 {
        return Err(validation_code);
    }
    if let Err(e) = rebind_database(&db, &root) {
        eprintln!("symhow --rebind: {}", e.0);
        return Err(2);
    }
    Ok(if standalone { None } else { Some(stripped) })
}
