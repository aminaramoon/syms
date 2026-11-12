use std::path::{Path, PathBuf};
use symhow::common::paths::SourcePaths;
use symhow::db::{Database, WritableDb};
use symhow::db::meta::{get_meta, set_meta};
use symhow::index::srctrldb_backend::{PendingTuRow, SrctrldbBackend};
use symhow::model::{FileFact, IndexBackend, IndexSummary, Name, NameElement, SymbolFact, SymbolKind, TuFactBatch};
use symhow::query::{resolve_file_path, QueryEngine, ResolveOutcome};
use symhow::cli::rebind::rebind_database;

fn dir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("symhow_portable_{}_{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}
fn text(p: &Path) -> String { p.to_string_lossy().into_owned() }
fn create_db(path: &Path, root: Option<&Path>) {
    let mut backend = SrctrldbBackend::new();
    backend.set_no_vacuum(true);
    backend.begin(&text(path)).unwrap();
    if let Some(root) = root { backend.set_source_root(&text(root)).unwrap(); }
    backend.start_transaction().unwrap();
    std::fs::create_dir_all(path.parent().unwrap().join("src")).unwrap();
    std::fs::write(path.parent().unwrap().join("src/a.cpp"), "int a() { return 1; }\n").unwrap();
    let mut batch = TuFactBatch::default();
    batch.files.push(FileFact {
        path: text(&path.parent().unwrap().join("src/a.cpp")),
        language: "cpp".into(), indexed: true, system: false, content_hash: String::new(),
    });
    batch.files.push(FileFact {
        path: "/external/library.hpp".into(),
        language: "cpp".into(), indexed: false, system: true, content_hash: String::new(),
    });
    backend.consume(&batch).unwrap();
    backend.checkpoint(&[PendingTuRow {
        path: text(&path.parent().unwrap().join("src/a.cpp")),
        args_hash: "hash".into(), status: "ok".into(), entry_count: 1,
    }]).unwrap();
    backend.finish(&mut IndexSummary::default()).unwrap();
}

#[test]
fn source_path_codec_is_component_aware_and_legacy_is_unchanged() {
    let paths = SourcePaths { root: "/work/repo".into() };
    assert_eq!(paths.encode("/work/repo/src/../a.cpp"), "a.cpp");
    assert_eq!(paths.encode("/work/repo-other/a.cpp"), "/work/repo-other/a.cpp");
    assert_eq!(paths.decode("src/a.cpp"), "/work/repo/src/a.cpp");
    assert_eq!(paths.decode("/usr/include/a.h"), "/usr/include/a.h");
    assert_eq!(SourcePaths::default().decode("relative.cpp"), "relative.cpp");
}

#[test]
fn backend_stores_relative_file_nodes_and_tus_runtime_decodes_and_invalidator_encodes() {
    let root = dir("backend");
    let path = root.join("index.srctrldb");
    create_db(&path, Some(&root));
    let db = Database::open(text(&path), 0).unwrap();
    assert_eq!(get_meta(db.conn(), "symhow_path_format").as_deref(), Some("relative-v1"));
    let stored: (i64, String, String) = db.conn().query_row(
        "SELECT f.id,f.path,n.serialized_name FROM file f JOIN node n ON n.id=f.id WHERE f.indexed=1",
        [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
    assert_eq!(stored.1, "src/a.cpp");
    assert!(stored.2.contains("src/a.cpp"));
    assert!(!stored.2.contains(&text(&root)));
    let tu: String = db.conn().query_row("SELECT path FROM symhow_tu", [], |r| r.get(0)).unwrap();
    assert_eq!(tu, "src/a.cpp");
    let content: String = db.conn().query_row("SELECT content FROM filecontent WHERE id=?1", [stored.0], |r| r.get(0)).unwrap();
    assert_eq!(content, "int a() { return 1; }\n");
    let absolute = text(&root.join("src/a.cpp"));
    let engine = QueryEngine::new(&db).unwrap();
    assert_eq!(engine.file_info(stored.0, false, false, None, None).unwrap().path, absolute);
    assert_eq!(engine.file_map().values().filter(|p| p.as_str() == "/external/library.hpp").count(), 1);
    let resolved = resolve_file_path(&db, &absolute).unwrap();
    assert_eq!(resolved.outcome, ResolveOutcome::Unique);
    assert_eq!(resolved.candidates[0].serialized_name, absolute);
    drop(engine);
    drop(db);
    let mut w = WritableDb::open(text(&path), true).unwrap();
    let plan = symhow::update::invalidator::compute_clear_set(&mut w, &[absolute.clone()], &[], &[absolute.clone()]).unwrap();
    assert_eq!(plan.cleared_paths, vec![absolute]);
    symhow::update::invalidator::invalidate(&mut w, &plan).unwrap();
    assert_eq!(w.conn().query_row("SELECT count(*) FROM symhow_tu", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn persistent_rebind_relocates_metadata_twice_and_leaves_external_paths_alone() {
    let old = dir("old root");
    let new = dir("new root");
    let last = dir("last root");
    let path = old.join("index.srctrldb");
    create_db(&path, Some(&old));
    let w = WritableDb::open(text(&path), false).unwrap();
    for (key, value) in [
        ("symhow_repo_root", text(&old)),
        ("symhow_cdb_path", text(&old.join("build/compile_commands.json"))),
        ("symhow_notes_dir", text(&old.join("notes"))),
        ("symhow_dirty_paths", serde_json::json!([text(&old.join("src/a.cpp")), "/external/z.cpp"]).to_string()),
        ("symhow_options_json", serde_json::json!({
            "remaps": [{"from":"/original", "to":text(&old)}],
            "include":[format!("{}/src/**", old.display())],
            "resource_dir":text(&old.join("clang")),
            "gcc_toolchain":"/external/gcc",
            "extra_args":[format!("-I{}/include",old.display()), format!("--sysroot={}/sysroot", old.display())]
        }).to_string()),
    ] { set_meta(&w, key, &value).unwrap(); }
    drop(w);
    rebind_database(&text(&path), &text(&new)).unwrap();
    rebind_database(&text(&path), &text(&last)).unwrap();
    let db = Database::open(text(&path), 0).unwrap();
    assert_eq!(get_meta(db.conn(), "symhow_source_root"), Some(text(&last)));
    assert_eq!(get_meta(db.conn(), "symhow_cdb_path"), Some(text(&last.join("build/compile_commands.json"))));
    let options: serde_json::Value = serde_json::from_str(&get_meta(db.conn(), "symhow_options_json").unwrap()).unwrap();
    assert_eq!(options["remaps"][0]["from"], "/original");
    assert_eq!(options["remaps"].as_array().unwrap().len(), 1);
    assert_eq!(options["remaps"][0]["to"], text(&old));
    let root_remaps: serde_json::Value = serde_json::from_str(&get_meta(db.conn(), "symhow_root_remaps").unwrap()).unwrap();
    assert_eq!(root_remaps.as_array().unwrap().len(), 2);
    for remap in root_remaps.as_array().unwrap() { assert_eq!(remap["to"], text(&last)); }
    assert_eq!(options["gcc_toolchain"], "/external/gcc");
    assert_eq!(options["extra_args"][0], format!("-I{}/include", last.display()));
    assert_eq!(QueryEngine::new(&db).unwrap().file_map().values().filter(|p| p.as_str() == text(&last.join("src/a.cpp"))).count(), 1);
    assert!(rebind_database(&text(&path), &text(&old.join("missing"))).is_err());
    assert_eq!(get_meta(db.conn(), "symhow_source_root"), Some(text(&last)));
    drop(db);
    // Returning to a historical root must not leave a mapping that moves
    // current paths away again during the next compilation-database load.
    rebind_database(&text(&path), &text(&old)).unwrap();
    let db = Database::open(text(&path), 0).unwrap();
    let roots: serde_json::Value = serde_json::from_str(
        &get_meta(db.conn(), "symhow_root_remaps").unwrap()).unwrap();
    for remap in roots.as_array().unwrap() {
        assert_ne!(remap["from"], text(&old));
        assert_eq!(remap["to"], text(&old));
    }
}

#[test]
fn rebind_refuses_legacy_in_progress_wrong_version_and_help_does_not_mutate() {
    let root = dir("refusal");
    let path = root.join("legacy.srctrldb");
    create_db(&path, None);
    assert!(rebind_database(&text(&path), &text(&root)).unwrap_err().to_string().contains("re-index"));
    let w = WritableDb::open(text(&path), false).unwrap();
    set_meta(&w, "symhow_path_format", "relative-v1").unwrap();
    assert!(rebind_database(&text(&path), &text(&root)).is_err());
    set_meta(&w, "symhow_source_root", &text(&root)).unwrap();
    set_meta(&w, "symhow_update_in_progress", "1").unwrap();
    assert!(rebind_database(&text(&path), &text(&root)).is_err());
    set_meta(&w, "symhow_update_in_progress", "0").unwrap();
    set_meta(&w, "storage_version", "24").unwrap();
    assert!(rebind_database(&text(&path), &text(&root)).is_err());
    let args = vec!["symhow", "--rebind", "/does/not/exist", "--help", "-d", path.to_str().unwrap()].into_iter().map(str::to_string).collect::<Vec<_>>();
    assert_eq!(symhow::cli::run(&args), 0);
    assert_eq!(get_meta(w.conn(), "storage_version").as_deref(), Some("24"));
}

#[test]
fn rebind_rejections_and_transaction_rollback_leave_database_unchanged() {
    let old = dir("rollback old");
    let new = dir("rollback new");
    let path = old.join("index.srctrldb");
    create_db(&path, Some(&old));
    let w = WritableDb::open(text(&path), false).unwrap();
    set_meta(&w, "symhow_repo_root", &text(&old)).unwrap();
    set_meta(&w, "symhow_options_json", "not json").unwrap();
    drop(w);
    let before = std::fs::read(&path).unwrap();
    assert!(rebind_database(&text(&path), &text(&new)).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!Path::new(&format!("{}.symhow-lock", path.display())).exists());
    for suffix in [".symhow-lock", "-journal", "-wal"] {
        let lock = format!("{}{}", path.display(), suffix);
        std::fs::write(&lock, "active").unwrap();
        assert!(rebind_database(&text(&path), &text(&new)).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_file(lock).unwrap();
    }
    let w = WritableDb::open(text(&path), false).unwrap();
    set_meta(&w, "symhow_options_json", "{}").unwrap();
    drop(w);
    let before = std::fs::read(&path).unwrap();
    for prefix in [vec!["index"], vec!["dbdiff"], vec!["notes", "fmt"],
        vec!["notes", "lint"], vec!["query", "unknown"], vec!["graph"], vec!["notes", "unknown"]] {
        let mut args = vec!["symhow".to_string()];
        args.extend(prefix.into_iter().map(str::to_string));
        args.extend(["--rebind".into(), text(&new), "-d".into(), text(&path)]);
        assert_ne!(symhow::cli::run(&args), 0);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn update_and_resume_preserve_minimal_failed_batches_for_missing_retry_tus() {
    use symhow::index::index_driver::{run_index, IndexRunOptions};
    use symhow::index::provenance::Provenance;
    use symhow::update::{run_update, UpdateRunOptions, UpdateSummary};
    use symhow::update::snapshot::{diff_snapshots, take_snapshot, SnapshotOptions};

    for interrupted in [false, true] {
        let root = dir(if interrupted { "missing resume" } else { "missing retry" });
        let path = root.join("index.srctrldb");
        let source = root.join("main.cpp");
        let missing = root.join("absent.cpp");
        std::fs::write(&source, "int main() { return 0; }\n").unwrap();
        let cdb = root.join("compile_commands.json");
        let entries: Vec<_> = [&source, &missing].into_iter().map(|file| {
            serde_json::json!({"directory": text(&root), "file": text(file),
                "arguments": ["clang++", "-std=c++20", text(file)]})
        }).collect();
        std::fs::write(&cdb, serde_json::to_vec(&entries).unwrap()).unwrap();
        let mut opts = IndexRunOptions::default();
        opts.compile_commands_path = text(&cdb);
        opts.output_path = text(&path);
        opts.quiet = true;
        opts.super_quiet = true;
        opts.commit_every = 1;
        opts.provenance = Some(Provenance {
            cdb_path: text(&cdb), repo_root: text(&root), ..Default::default()
        });
        let fresh = root.join("fresh.srctrldb");
        let mut fresh_opts = opts.clone();
        fresh_opts.output_path = text(&fresh);
        assert_eq!(run_index(&fresh_opts, None), 0);
        if interrupted { opts.stop_after = 1; }
        assert_eq!(run_index(&opts, None), if interrupted { 130 } else { 0 });

        let mut update = UpdateRunOptions::default();
        update.db_path = text(&path);
        // Explicit files allow a non-git corpus; the missing TU must be
        // selected independently through failed-status/resume retry logic.
        update.explicit_files.push(text(&source));
        let mut summary = UpdateSummary::default();
        assert_eq!(run_update(&update, Some(&mut summary)), 0);
        assert_eq!(summary.tus_retried, 1);
        assert_eq!(summary.tus_to_index, 2);
        assert_eq!(summary.skipped_missing, 0);
        let db = Database::open(text(&path), 0).unwrap();
        let status: String = db.conn().query_row(
            "SELECT status FROM symhow_tu WHERE path='absent.cpp'", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "failed");
        let a = take_snapshot(&text(&path), &SnapshotOptions::default()).unwrap();
        let b = take_snapshot(&text(&fresh), &SnapshotOptions::default()).unwrap();
        let diff = diff_snapshots(&a, &b, &[]);
        assert!(diff.only_in_a.is_empty(), "update-only: {:?}", diff.only_in_a);
        assert!(diff.only_in_b.is_empty(), "fresh-only: {:?}", diff.only_in_b);
    }
}

#[test]
fn backend_consume_never_reads_side_connection_after_writer_cache_spills() {
    let root = dir("writer spill");
    let path = root.join("index.srctrldb");
    create_db(&path, Some(&root));
    let mut backend = SrctrldbBackend::new();
    backend.set_no_vacuum(true);
    backend.begin(&text(&path)).unwrap();
    backend.start_transaction().unwrap();
    // More than SQLite's default page cache: a rollback-journal writer
    // spills dirty pages and holds EXCLUSIVE until the next checkpoint.
    let mut batch = TuFactBatch::default();
    for i in 0..8000 {
        let name = format!("spill_{}_{}", i, "x".repeat(512));
        batch.symbols.push(SymbolFact {
            key: name.clone(),
            name: Name { delimiter: "::".into(), elements: vec![NameElement {
                name, ..Default::default()
            }] },
            kind: SymbolKind::Function,
            ..Default::default()
        });
    }
    backend.consume(&batch).unwrap();
    let source = root.join("src/after.cpp");
    std::fs::write(&source, "int after();\n").unwrap();
    let mut files = TuFactBatch::default();
    for source in [source, root.join("src/a.cpp")] {
        files.files.push(FileFact { path: text(&source), language: "cpp".into(),
            indexed: true, system: false, content_hash: "hash".into() });
    }
    backend.consume(&files).unwrap();
    backend.checkpoint(&[]).unwrap();
    backend.finish(&mut IndexSummary::default()).unwrap();
    let db = Database::open(text(&path), 0).unwrap();
    let content: String = db.conn().query_row(
        "SELECT c.content FROM filecontent c JOIN file f ON f.id=c.id WHERE f.path='src/after.cpp'",
        [], |row| row.get(0)).unwrap();
    assert_eq!(content, "int after();\n");
}
