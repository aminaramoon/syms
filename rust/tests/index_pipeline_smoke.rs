//! End-to-end smoke tests of the index write path: build tiny compile
//! databases, run the full `run_index` pipeline into an output .srctrldb,
//! then verify the on-disk database (Sourcetrail schema + symhow-private
//! side tables) with rusqlite.
//!
//! The TUs are deliberately trivial (`int f() { return 1; }` style, no
//! system includes) so they parse without a resource dir.

use std::path::PathBuf;

use symhow::index::index_driver::{run_index, IndexRunOptions};
use symhow::index::provenance::Provenance;
use symhow::model::IndexSummary;

fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "symhow_index_smoke_{}_{}",
        std::process::id(),
        name
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_tu(dir: &std::path::Path, file: &str, contents: &str) -> String {
    let path = dir.join(file);
    std::fs::write(&path, contents).unwrap();
    path.to_string_lossy().to_string()
}

/// Writes a compile_commands.json for `files` (already on disk under `dir`)
/// and returns its path.
fn write_cdb(dir: &std::path::Path, files: &[String]) -> PathBuf {
    let dir_str = dir.to_string_lossy().to_string();
    let entries: Vec<serde_json::Value> = files
        .iter()
        .map(|file| {
            serde_json::json!({
                "directory": dir_str,
                "file": file,
                "arguments": ["clang++", "-x", "c++", "-std=c++17", file],
            })
        })
        .collect();
    let cdb = dir.join("compile_commands.json");
    std::fs::write(&cdb, serde_json::Value::Array(entries).to_string()).unwrap();
    cdb
}

fn query_scalar(conn: &rusqlite::Connection, sql: &str) -> String {
    conn.query_row(sql, [], |row| row.get::<_, String>(0)).unwrap()
}

fn query_count(conn: &rusqlite::Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {}", table), [], |row| row.get(0))
        .unwrap()
}

/// The shared post-run DB assertions: schema version, node/edge rows,
/// symhow_tu contents, kind-merge upgrades, and the side tables.
fn assert_database_contents(out: &std::path::Path, tu_paths: &[String], expected_status: &str) {
    let conn = rusqlite::Connection::open(out).unwrap();

    // Sourcetrail schema version pinned by the vendored writer.
    assert_eq!(
        query_scalar(&conn, "SELECT value FROM meta WHERE key = 'storage_version';"),
        "25"
    );
    // The in-progress marker must be cleared by finish().
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM meta WHERE key = 'symhow_index_in_progress';",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );

    // Nodes: a.cpp's f + main, b.cpp's g, plus file nodes -- at least 3.
    let nodes = query_count(&conn, "node");
    assert!(nodes >= 3, "expected at least 3 nodes, got {}", nodes);
    // Edges: at least the CALL edge main -> f.
    let edges = query_count(&conn, "edge");
    assert!(edges >= 1, "expected at least 1 edge, got {}", edges);
    // File rows for both sources.
    let files = query_count(&conn, "file");
    assert!(files >= 2, "expected at least 2 file rows, got {}", files);

    // node.type must have been upgraded from UNKNOWN by the flushPending
    // kind-merge UPDATE (FUNCTION = 1 << 12): f, main, g.
    let function_nodes = conn
        .query_row(
            "SELECT COUNT(*) FROM node WHERE type = ?1;",
            [symhow::db::enums::node_kind::FUNCTION],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert!(
        function_nodes >= 3,
        "expected at least 3 FUNCTION nodes, got {}",
        function_nodes
    );
    // No symbol node may stay UNKNOWN after the flush.
    let unknown_nodes = conn
        .query_row(
            "SELECT COUNT(*) FROM node WHERE type = ?1;",
            [symhow::db::enums::node_kind::UNKNOWN],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(unknown_nodes, 0, "kind-merge left a node as UNKNOWN");

    // symbol.definition_kind: f/main/g are explicit definitions (EXPLICIT = 2).
    let explicit = conn
        .query_row(
            "SELECT COUNT(*) FROM symbol WHERE definition_kind = ?1;",
            [symhow::db::enums::definition_kind::EXPLICIT],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert!(explicit >= 3, "expected at least 3 explicit symbols, got {}", explicit);

    // symhow_symbol_def: (symbol, declaring file) pairs from declFiles.
    let symbol_defs = query_count(&conn, "symhow_symbol_def");
    assert!(symbol_defs >= 3, "expected at least 3 symhow_symbol_def rows, got {}", symbol_defs);

    // symhow_file_fingerprint: one per indexed file with parsed bytes.
    let fingerprints = query_count(&conn, "symhow_file_fingerprint");
    assert!(fingerprints >= 2, "expected at least 2 fingerprints, got {}", fingerprints);

    // symhow_tu: exactly one row per CDB path, with a resolved file node id.
    let tu_rows: Vec<(String, Option<i64>, String)> = conn
        .prepare("SELECT path, file_node_id, status FROM symhow_tu ORDER BY path;")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(
        tu_rows.len(),
        tu_paths.len(),
        "expected {} symhow_tu rows, got {:?}",
        tu_paths.len(),
        tu_rows
    );
    for path in tu_paths {
        assert!(
            tu_rows.iter().any(|r| r.0 == *path),
            "missing symhow_tu row for {}: {:?}",
            path,
            tu_rows
        );
    }
    for (path, file_node_id, status) in &tu_rows {
        assert!(file_node_id.is_some(), "symhow_tu.file_node_id is NULL for {}", path);
        assert_eq!(status, expected_status, "symhow_tu.status for {} is {}", path, status);
    }

    // symhow_touched must exist and cover the produced nodes.
    let touched = query_count(&conn, "symhow_touched");
    assert!(touched >= 3, "expected at least 3 touched nodes, got {}", touched);
}

#[test]
fn run_index_two_file_cdb_produces_queryable_db() {
    let dir = fresh_dir("sequential");
    let a = write_tu(&dir, "a.cpp", "int f() { return 1; }\nint main() { return f(); }\n");
    let b = write_tu(&dir, "b.cpp", "int g() { return 2; }\n");
    let cdb = write_cdb(&dir, &[a.clone(), b.clone()]);

    let out = dir.join("out.srctrldb");
    let mut options = IndexRunOptions::default();
    options.compile_commands_path = cdb.to_string_lossy().to_string();
    options.output_path = out.to_string_lossy().to_string();
    options.quiet = true;

    let mut summary = IndexSummary::default();
    let code = run_index(&options, Some(&mut summary));
    assert_eq!(code, 0, "run_index exited non-zero; summary: {:?}", summary);
    assert_eq!(summary.tus_total, 2);
    assert_eq!(summary.tus_attempted, 2);
    assert_eq!(summary.tus_with_content, 2);
    assert_eq!(summary.tus_failed, 0);

    assert!(out.exists(), "output database was not created");
    assert_database_contents(&out, &[a, b], "ok");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_index_parallel_with_checkpoints_matches_sequential_db() {
    let dir = fresh_dir("parallel");
    let a = write_tu(&dir, "a.cpp", "int f() { return 1; }\nint main() { return f(); }\n");
    let b = write_tu(&dir, "b.cpp", "int g() { return 2; }\n");
    let cdb = write_cdb(&dir, &[a.clone(), b.clone()]);

    let out = dir.join("out.srctrldb");
    let mut options = IndexRunOptions::default();
    options.compile_commands_path = cdb.to_string_lossy().to_string();
    options.output_path = out.to_string_lossy().to_string();
    options.quiet = true;
    // -j 4 (worker pool + reorder buffer) with a checkpoint after every TU.
    options.parallel = 4;
    options.commit_every = 1;

    let mut summary = IndexSummary::default();
    let code = run_index(&options, Some(&mut summary));
    assert_eq!(code, 0, "parallel run_index exited non-zero; summary: {:?}", summary);
    assert_eq!(summary.tus_with_content, 2);

    assert_database_contents(&out, &[a, b], "ok");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stop_after_interrupts_exits_130_and_resumes_with_force() {
    let dir = fresh_dir("interrupt");
    let a = write_tu(&dir, "a.cpp", "int f() { return 1; }\nint main() { return f(); }\n");
    let b = write_tu(&dir, "b.cpp", "int g() { return 2; }\n");
    let cdb = write_cdb(&dir, &[a.clone(), b.clone()]);

    let out = dir.join("out.srctrldb");
    let out_str = out.to_string_lossy().to_string();
    let cdb_str = cdb.to_string_lossy().to_string();

    // Interrupt after 1 of 2 TUs, with provenance so the in-progress marker
    // is written up front.
    let mut options = IndexRunOptions::default();
    options.compile_commands_path = cdb_str.clone();
    options.output_path = out_str.clone();
    options.quiet = true;
    options.stop_after = 1;
    options.provenance = Some(Provenance::default());
    let mut summary = IndexSummary::default();
    let code = run_index(&options, Some(&mut summary));
    assert_eq!(code, 130, "interrupted run should exit 130; summary: {:?}", summary);
    // Faithful to the C++: summaryOut is only filled on the success path --
    // an interrupted run leaves it untouched (default).
    assert_eq!(summary.tus_attempted, 0);

    {
        let conn = rusqlite::Connection::open(&out).unwrap();
        // finish_interrupted deliberately leaves symhow_index_in_progress set.
        assert_eq!(
            query_scalar(&conn, "SELECT value FROM meta WHERE key = 'symhow_index_in_progress';"),
            "1"
        );
        // Exactly the interrupted path's last-entry row is written; the
        // never-consumed second path must have NO row at all.
        assert_eq!(query_count(&conn, "symhow_tu"), 1);
        // Reader indices and ANALYZE are deferred to the resume -- but the
        // flushed content itself is durable.
        assert!(query_count(&conn, "node") >= 2);
    }

    // Re-run without --force against the interrupted DB: exit 2, with the
    // "interrupted index run" recovery hint.
    let mut options = IndexRunOptions::default();
    options.compile_commands_path = cdb_str.clone();
    options.output_path = out_str.clone();
    options.quiet = true;
    let code = run_index(&options, None);
    assert_eq!(code, 2, "re-run over an interrupted DB without --force should exit 2");

    // Re-run with --force: a clean full index, marker cleared.
    let mut options = IndexRunOptions::default();
    options.compile_commands_path = cdb_str;
    options.output_path = out_str;
    options.quiet = true;
    options.force = true;
    let code = run_index(&options, None);
    assert_eq!(code, 0, "forced re-run should exit 0");

    assert_database_contents(&out, &[a, b], "ok");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_index_exits_5_when_no_tu_produces_content() {
    let dir = fresh_dir("nocontent");
    // A CDB entry whose file does not exist: every TU fails to produce
    // content, so the run completes but exits 5.
    let bogus = dir.join("nonexistent.cpp").to_string_lossy().to_string();
    let cdb = write_cdb(&dir, &[bogus.clone()]);

    let out = dir.join("out.srctrldb");
    let mut options = IndexRunOptions::default();
    options.compile_commands_path = cdb.to_string_lossy().to_string();
    options.output_path = out.to_string_lossy().to_string();
    options.quiet = true;

    let mut summary = IndexSummary::default();
    let code = run_index(&options, Some(&mut summary));
    assert_eq!(code, 5, "no-content run should exit 5; summary: {:?}", summary);
    assert_eq!(summary.tus_total, 1);
    assert_eq!(summary.tus_with_content, 0);
    assert_eq!(summary.tus_failed, 1);
    assert_eq!(summary.failed_tus, vec![bogus]);

    // finish() still ran (reader indices, marker cleared, side tables).
    let conn = rusqlite::Connection::open(&out).unwrap();
    assert_eq!(
        query_scalar(&conn, "SELECT value FROM meta WHERE key = 'storage_version';"),
        "25"
    );
    assert_eq!(query_count(&conn, "symhow_tu"), 1);

    let _ = std::fs::remove_dir_all(&dir);
}
