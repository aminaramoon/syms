//! Port of test/unit/test_resume_fixture.cpp: Phase 8 (crash-safe,
//! resumable indexing) tests against the real synthetic fixture CDB/DB
//! (env SYMHOW_FIXTURE_CDB / SYMHOW_FIXTURE_DB, falling back to the C++
//! tree's build/ artifacts -- the same env-var pattern as
//! tests/query_fixture.rs).
//!
//! The C++ file's SYMHOW_BINARY-guarded CLI sub-checks are handled at the
//! library level where the Rust port owns them: the "`symhow index` on an
//! in-progress db refuses with exit 2" check is a plain run_index() call
//! here (the Rust run_index owns that refusal), and the
//! `symhow query stats` CLI-verb smoke check is left to the CLI layer's
//! own tests.

use std::path::Path;

use symhow::db::meta::get_meta;
use symhow::db::Database;
use symhow::index::index_driver::{run_index, IndexRunOptions};
use symhow::index::provenance::{read_provenance, Provenance};
use symhow::update::snapshot::{diff_snapshots, take_snapshot, Snapshot, SnapshotOptions};
use symhow::update::{run_update, UpdateRunOptions, UpdateSummary};

fn fixture_db_path() -> String {
    std::env::var("SYMHOW_FIXTURE_DB").unwrap_or_else(|_| "../build/fixture.srctrldb".to_string())
}

fn fixture_cdb_path() -> String {
    std::env::var("SYMHOW_FIXTURE_CDB").unwrap_or_else(|_| "../build/fixture_build/compile_commands.json".to_string())
}

fn test_output_dir(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("symhow_resume_test_{}_{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.to_string_lossy().into_owned()
}

// The pre-built canonical fixture.srctrldb already carries real provenance
// (absolute cdbPath/repoRoot/repoCommit, discovered at fixture-build time)
// -- reusing it (rather than synthesizing a fresh Provenance here) means
// every run_index() call below indexes with EXACTLY the options/repo
// baseline fixture.srctrldb itself was built with, so it can be used as the
// "what a fresh, uninterrupted index produces" ground truth for
// dbdiff-style comparisons.
fn fixture_provenance() -> Provenance {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    read_provenance(db.conn()).expect("fixture db has provenance")
}

fn node_ids_and_names(db_path: &str) -> Vec<(i64, String)> {
    let db = Database::open(db_path, 0).unwrap();
    let mut rows: Vec<(i64, String)> = Vec::new();
    db.run("SELECT id, serialized_name FROM node ORDER BY id", [], |row| {
        let id: i64 = row.get(0)?;
        let name: Option<String> = row.get(1)?;
        rows.push((id, name.unwrap_or_default()));
        Ok(())
    })
    .unwrap();
    rows
}

fn index_fixture(output_path: &str, commit_every: i32, stop_after: i64) -> i32 {
    let prov = fixture_provenance();
    let mut opts = IndexRunOptions::default();
    opts.compile_commands_path = fixture_cdb_path();
    opts.output_path = output_path.to_string();
    opts.force = true;
    opts.quiet = true;
    opts.super_quiet = true;
    opts.symhow_version = "test-version".to_string();
    opts.commit_every = commit_every;
    opts.stop_after = stop_after;
    opts.db_options = prov.db_options.clone();
    opts.sanitizer_options = prov.sanitizer_options.clone();
    opts.system_file_rows = prov.system_file_rows;
    opts.provenance = Some(prov);
    run_index(&opts, None)
}

fn read_meta(db_path: &str, key: &str) -> Option<String> {
    let db = Database::open(db_path, 0).unwrap();
    get_meta(db.conn(), key)
}

fn snapshot_of(path: &str) -> Snapshot {
    take_snapshot(path, &SnapshotOptions::default()).unwrap()
}

fn assert_snapshots_equal(a: &str, b: &str) {
    let diff = diff_snapshots(&snapshot_of(a), &snapshot_of(b), &[]);
    assert!(diff.only_in_a.is_empty(), "only in {}: {:?}", a, diff.only_in_a);
    assert!(diff.only_in_b.is_empty(), "only in {}: {:?}", b, diff.only_in_b);
}

#[test]
fn resume_commit_every_1_vs_25_vs_0_produce_an_identical_database() {
    let dir = test_output_dir("ce");
    let db_a = format!("{}/resume_test_ce1.srctrldb", dir);
    let db_b = format!("{}/resume_test_ce25.srctrldb", dir);
    let db_c = format!("{}/resume_test_ce0.srctrldb", dir);

    assert_eq!(index_fixture(&db_a, 1, -1), 0);
    assert_eq!(index_fixture(&db_b, 25, -1), 0);
    assert_eq!(index_fixture(&db_c, 0, -1), 0);

    assert_snapshots_equal(&db_a, &db_b);
    assert_snapshots_equal(&db_a, &db_c);

    let nodes_a = node_ids_and_names(&db_a);
    assert!(!nodes_a.is_empty());
    // Checkpointing must not change node ids or their assignment order --
    // ties (equal-rank node.type upgrades) are decided identically whether
    // flushed once (commit_every=0) or once per TU (commit_every=1).
    assert_eq!(nodes_a, node_ids_and_names(&db_b));
    assert_eq!(nodes_a, node_ids_and_names(&db_c));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resume_stop_after_interrupts_cleanly_and_update_finishes_it_matching_a_fresh_index() {
    let dir = test_output_dir("interrupted");
    let db_path = format!("{}/resume_test_interrupted.srctrldb", dir);
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(format!("{}-journal", db_path));

    // The fixture CDB has 8 entries (see test_incremental's own comments) --
    // stop after 3, checkpointing every 2, so the interrupt lands strictly
    // after a checkpoint has already flushed once.
    let rc = index_fixture(&db_path, 2, 3);
    assert_eq!(rc, 130);

    // No hot journal left -- the writer connection was closed cleanly.
    assert!(!Path::new(&format!("{}-journal", db_path)).exists());

    // Openable read-only (verifies no corruption / no hanging lock).
    {
        let db = Database::open(&db_path, 0).unwrap();
        let mut tu_count = 0i64;
        db.run("SELECT count(*) FROM symhow_tu", [], |row| {
            tu_count = row.get(0)?;
            Ok(())
        })
        .unwrap();
        assert_eq!(tu_count, 3);
    }

    let in_progress = read_meta(&db_path, "symhow_index_in_progress");
    assert!(in_progress.is_some());
    assert_eq!(in_progress.as_deref(), Some("1"));

    // `symhow index` (no --force) on an in-progress db refuses with exit 2
    // and points at `symhow update`/--force, rather than the generic
    // "already exists" exit 1. (The C++ does this through the CLI binary;
    // here the Rust run_index owns the identical refusal.)
    {
        let mut opts = IndexRunOptions::default();
        opts.compile_commands_path = fixture_cdb_path();
        opts.output_path = db_path.clone();
        opts.quiet = true;
        let index_rc = run_index(&opts, None);
        assert_eq!(index_rc, 2);
    }

    // Resume via run_update() -- no --files, so this exercises the real
    // "no symhow_tu row at all" retry-set path (Phase 8 resume), not just
    // a git-diff-driven partial update.
    let mut upd_opts = UpdateRunOptions::default();
    upd_opts.db_path = db_path.clone();
    upd_opts.symhow_version = "test-version".to_string();
    let mut summary = UpdateSummary::default();
    let upd_rc = run_update(&upd_opts, Some(&mut summary));
    assert_eq!(upd_rc, 0);
    // The never-indexed CDB ENTRIES: the fixture CDB has 8 entries for 7
    // distinct paths (templates.cpp is compiled twice); the first 3
    // consumed by the interrupted run (ns.cpp, overloads.cpp, chain.cpp)
    // are neither of templates.cpp's two entries, so both of its entries
    // -- plus tu_a.cpp, tu_b.cpp, cfile.c -- are still pending: 8 total - 3
    // already indexed = 5.
    assert_eq!(summary.tus_to_index, 5);

    let in_progress_after = read_meta(&db_path, "symhow_index_in_progress");
    assert!(in_progress_after.is_none());

    // The resumed-then-completed db must be logically identical to the
    // canonical fresh fixture.srctrldb -- same CDB, same options, same repo
    // baseline (see fixture_provenance()'s comment).
    assert_snapshots_equal(&db_path, &fixture_db_path());

    let final_db = Database::open(&db_path, 0).unwrap();
    let mut final_tu_count = 0i64;
    final_db.run("SELECT count(*) FROM symhow_tu", [], |row| {
        final_tu_count = row.get(0)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(final_tu_count, 7);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resume_stop_after_between_a_duplicated_paths_two_entries_leaves_no_row_and_update_completes_it() {
    // Regression test for the duplicate-CDB-entry + interrupted-index
    // fragility: IndexDriver only ever UPSERTs a path's symhow_tu row once
    // its LAST CDB entry (in CDB order) has been consumed -- never on an
    // earlier one -- so an interrupted run that stops strictly BETWEEN two
    // entries of the SAME duplicated path must leave that path with NO
    // symhow_tu row at all (not a row that wrongly claims it's done).
    //
    // The fixture CDB is, in CDB order: ns.cpp, overloads.cpp, chain.cpp,
    // templates.cpp (first entry), tu_a.cpp, tu_b.cpp, cfile.c,
    // templates.cpp (second, duplicated entry) -- 8 entries, 7 distinct
    // paths. --stop-after 4 therefore consumes entries [0..3] -- through
    // templates.cpp's FIRST entry -- and stops strictly before its SECOND
    // entry is ever reached: exactly the "interrupted between two entries
    // of a duplicated path" scenario.
    let dir = test_output_dir("dup_split");
    let db_path = format!("{}/resume_test_dup_split.srctrldb", dir);
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(format!("{}-journal", db_path));

    let rc = index_fixture(&db_path, 2, 4);
    assert_eq!(rc, 130);

    {
        let db = Database::open(&db_path, 0).unwrap();

        let mut tu_count = 0i64;
        db.run("SELECT count(*) FROM symhow_tu", [], |row| {
            tu_count = row.get(0)?;
            Ok(())
        })
        .unwrap();
        // ns.cpp, overloads.cpp, chain.cpp are each single-entry paths and
        // complete normally; templates.cpp's first entry was consumed but
        // -- the regression this pins -- must NOT have produced a row.
        assert_eq!(tu_count, 3);

        let mut have_templates_row = false;
        db.run("SELECT path FROM symhow_tu", [], |row| {
            let path: Option<String> = row.get(0)?;
            if path.unwrap_or_default().ends_with("templates.cpp") {
                have_templates_row = true;
            }
            Ok(())
        })
        .unwrap();
        assert!(!have_templates_row);
    }

    let in_progress = read_meta(&db_path, "symhow_index_in_progress");
    assert!(in_progress.is_some());
    assert_eq!(in_progress.as_deref(), Some("1"));

    // Resume via run_update() -- no --files, so this exercises the real
    // "no symhow_tu row at all" retry-set path (Phase 8 resume). Because
    // templates.cpp has NO row at all, resume must pick up its path and
    // (via select_entries(), which selects whole paths, not individual
    // entries) re-consume BOTH of its CDB entries -- including
    // re-processing the one already consumed (and checkpointed) before the
    // interrupt, which is harmless (the backend's node/symbol dedup is
    // idempotent for identical facts).
    let mut upd_opts = UpdateRunOptions::default();
    upd_opts.db_path = db_path.clone();
    upd_opts.symhow_version = "test-version".to_string();
    let mut summary = UpdateSummary::default();
    let upd_rc = run_update(&upd_opts, Some(&mut summary));
    assert_eq!(upd_rc, 0);

    let in_progress_after = read_meta(&db_path, "symhow_index_in_progress");
    assert!(in_progress_after.is_none());

    // The resumed-then-completed db must be logically IDENTICAL to a fresh
    // full index -- the actual correctness bar this regression test exists
    // to enforce (a stale/missing symhow_tu row for the split path could
    // silently leave templates.cpp's second entry's facts out forever).
    assert_snapshots_equal(&db_path, &fixture_db_path());

    let final_db = Database::open(&db_path, 0).unwrap();
    let mut final_tu_count = 0i64;
    final_db.run("SELECT count(*) FROM symhow_tu", [], |row| {
        final_tu_count = row.get(0)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(final_tu_count, 7);

    let mut templates_entry_count: i64 = -1;
    final_db
        .run("SELECT entry_count FROM symhow_tu WHERE path LIKE '%templates.cpp'", [], |row| {
            templates_entry_count = row.get(0)?;
            Ok(())
        })
        .unwrap();
    assert_eq!(templates_entry_count, 2);

    let _ = std::fs::remove_dir_all(&dir);
}
