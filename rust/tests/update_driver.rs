//! Port of test/unit/test_update_driver.cpp (fixture-dependent): exercises
//! `symhow::update::run_update()` against COPIES of the real synthetic
//! fixture database. The fixture CDB comes from env `SYMHOW_FIXTURE_CDB`
//! (falling back to ../cpp/build/fixture_build/compile_commands.json, the
//! C++ tree's fixture build), and the fixture DB from env
//! `SYMHOW_FIXTURE_DB` (../cpp/build/fixture.srctrldb) -- the same env-var
//! pattern as tests/query_fixture.rs.
//!
//! The C++ file's SYMHOW_BINARY-guarded CLI smoke test (`symhow update
//! --json`) is not ported: it shells out to the finished CLI binary, whose
//! `update` wiring is the CLI layer's own, separately-tested concern.

use std::path::Path;

use symhow::db::meta::{get_meta, set_meta};
use symhow::db::WritableDb;
use symhow::update::{diff_snapshots, take_snapshot, Snapshot, SnapshotOptions};
use symhow::update::{run_update, UpdateRunOptions, UpdateSummary};

fn fixture_db_path() -> String {
    std::env::var("SYMHOW_FIXTURE_DB").unwrap_or_else(|_| "../cpp/build/fixture.srctrldb".to_string())
}

fn fixture_cdb_path() -> String {
    std::env::var("SYMHOW_FIXTURE_CDB").unwrap_or_else(|_| "../cpp/build/fixture_build/compile_commands.json".to_string())
}

/// The symhow source tree's test/ directory (C++ SYMHOW_TEST_SOURCE_DIR).
fn source_dir() -> String {
    std::env::var("SYMHOW_TEST_SOURCE_DIR").unwrap_or_else(|_| "../cpp/test".to_string())
}

fn copy_fixture(tag: &str) -> String {
    let path = std::env::temp_dir().join(format!("symhow_test_update_driver_{}.srctrldb", tag));
    let path_str = path.to_string_lossy().into_owned();
    let _ = std::fs::remove_file(&path);
    std::fs::copy(fixture_db_path(), &path).unwrap();
    path_str
}

fn synth_path(relative: &str) -> String {
    let p = Path::new(&source_dir()).join("fixtures").join("synthetic").join(relative);
    // std::filesystem::absolute(...).lexically_normal()
    let abs = if p.is_absolute() {
        p
    } else {
        std::env::current_dir().unwrap().join(p)
    };
    symhow::common::paths::lexical_normalize(&abs.to_string_lossy())
}

/// Not a real hash -- just size + a checksum of the bytes, cheap and
/// sufficient to detect "did this file change at all" for the
/// byte-identical --dry-run assertions (mirrors the C++ fileHash()).
fn file_hash(path: &str) -> u64 {
    let bytes = std::fs::read(path).unwrap();
    let mut acc: u64 = 1469598103934665603;
    for b in bytes {
        acc ^= b as u64;
        acc = acc.wrapping_mul(1099511628211);
    }
    acc
}

fn node_id_by_serialized_name_like(db_path: &str, pattern: &str) -> i64 {
    let db = WritableDb::open(db_path, false).unwrap();
    let mut id: i64 = -1;
    db.query_bound(
        "SELECT id FROM node WHERE serialized_name LIKE ?1 LIMIT 1",
        [pattern],
        |row| {
            id = row.get(0)?;
            Ok(())
        },
    )
    .unwrap();
    id
}

/// (args_hash, entry_count) of the symhow_tu row whose path ends with
/// `suffix` -- used to prove a duplicated path's row is left completely
/// untouched by an update that never re-indexes it.
fn symhow_tu_args_hash_and_count(db_path: &str, suffix: &str) -> (String, i64) {
    let db = WritableDb::open(db_path, false).unwrap();
    let mut result: Option<(String, i64)> = None;
    db.query_bound("SELECT path, args_hash, entry_count FROM symhow_tu", [], |row| {
        let path: Option<String> = row.get(0)?;
        let path = path.unwrap_or_default();
        if path.len() >= suffix.len() && path.ends_with(suffix) {
            let hash: Option<String> = row.get(1)?;
            let count: i64 = row.get(2)?;
            result = Some((hash.unwrap_or_default(), count));
        }
        Ok(())
    })
    .unwrap();
    result.expect("expected a symhow_tu row ending with the given suffix")
}

fn snapshot_of(path: &str) -> Snapshot {
    take_snapshot(path, &SnapshotOptions::default()).unwrap()
}

fn assert_snapshots_equal(a: &str, b: &str) {
    let diff = diff_snapshots(&snapshot_of(a), &snapshot_of(b), &[]);
    assert!(diff.only_in_a.is_empty(), "only in {}: {:?}", a, diff.only_in_a);
    assert!(diff.only_in_b.is_empty(), "only in {}: {:?}", b, diff.only_in_b);
}

fn base_options(db_path: &str) -> UpdateRunOptions {
    let mut opts = UpdateRunOptions::default();
    opts.db_path = db_path.to_string();
    opts.cdb_path = fixture_cdb_path();
    opts.symhow_version = "test".to_string();
    opts
}

#[test]
fn update_driver_files_chain_cpp_reproduces_fresh_fixture_db_exactly() {
    let path = copy_fixture("chain_reproduce");
    let shape_id_before = node_id_by_serialized_name_like(&path, "%Shape%");
    assert!(shape_id_before >= 0);

    let mut opts = base_options(&path);
    opts.explicit_files = vec![synth_path("src/chain.cpp")];

    let mut summary = UpdateSummary::default();
    let rc = run_update(&opts, Some(&mut summary));
    assert_eq!(rc, 0);

    assert_snapshots_equal(&path, &fixture_db_path());

    let shape_id_after = node_id_by_serialized_name_like(&path, "%Shape%");
    assert_eq!(shape_id_after, shape_id_before);
}

#[test]
fn update_driver_second_no_op_run_retries_nothing() {
    // Phase 4: symhow_tu.status replaces the old (Phase 3) "no outgoing
    // INCLUDE edge" retry heuristic, which used to re-retry every
    // include-free TU on EVERY run, forever. With precise per-TU status
    // tracking, a second run that only touches chain.cpp again must retry
    // nothing and re-index exactly the one requested TU.
    let path = copy_fixture("chain_second_run");

    let mut opts = base_options(&path);
    opts.explicit_files = vec![synth_path("src/chain.cpp")];

    let mut first = UpdateSummary::default();
    assert_eq!(run_update(&opts, Some(&mut first)), 0);

    let mut second = UpdateSummary::default();
    assert_eq!(run_update(&opts, Some(&mut second)), 0);
    assert_eq!(second.tus_retried, 0);
    assert_eq!(second.tus_to_index, 1);

    assert_snapshots_equal(&path, &fixture_db_path());
}

#[test]
fn update_driver_duplicate_cdb_entry_regression_unrelated_update_never_false_flags() {
    // The bug this fixes: symhow_tu is keyed by PATH, but the fixture's CDB
    // has TWO entries for templates.cpp -- one file, two different sets of
    // compiler flags. Before the combined per-path hash fix, updating an
    // UNRELATED file (chain.cpp here) would flag templates.cpp as
    // "flag changed" and re-index its entries on EVERY run.
    let path = copy_fixture("dup_entry_unrelated_update");

    let templates_row_before = symhow_tu_args_hash_and_count(&path, "templates.cpp");
    assert_eq!(templates_row_before.1, 2); // entry_count -- see the fixture's dup target

    let mut opts = base_options(&path);
    opts.explicit_files = vec![synth_path("src/chain.cpp")]; // unrelated to templates.cpp's duplicate entries

    let mut first = UpdateSummary::default();
    assert_eq!(run_update(&opts, Some(&mut first)), 0);
    // No path (in particular not templates.cpp) is reported as
    // flag-changed by an update that never touched it.
    assert_eq!(first.flag_changed_count, 0);
    // Only chain.cpp's own single CDB entry is re-indexed.
    assert_eq!(first.tus_to_index, 1);
    // templates.cpp's symhow_tu row (combined hash + entry_count) is untouched.
    assert_eq!(symhow_tu_args_hash_and_count(&path, "templates.cpp"), templates_row_before);

    // --files always forces its own target to be treated as Modified
    // (that's what --files is FOR), so a second identical --files chain.cpp
    // run legitimately re-indexes chain.cpp again too. What must NOT happen,
    // on this or any other run, is templates.cpp's path getting dragged in
    // as a false "flag changed".
    let mut second = UpdateSummary::default();
    assert_eq!(run_update(&opts, Some(&mut second)), 0);
    assert_eq!(second.flag_changed_count, 0);
    assert_eq!(second.tus_to_index, 1);
    assert_eq!(symhow_tu_args_hash_and_count(&path, "templates.cpp"), templates_row_before);

    // The resulting db must still be logically identical to the canonical
    // fresh fixture (templates.cpp's symhow_tu row -- combined hash,
    // entry_count=2 -- was never touched by either update, and correctly
    // still describes both of its CDB entries).
    assert_snapshots_equal(&path, &fixture_db_path());
}

#[test]
fn update_driver_files_shapes_hpp_multi_tu_closure_reproduces_fresh_fixture_db() {
    let path = copy_fixture("shapes_reproduce");

    let mut opts = base_options(&path);
    opts.explicit_files = vec![synth_path("include/shapes.hpp")];

    let mut summary = UpdateSummary::default();
    let rc = run_update(&opts, Some(&mut summary));
    assert_eq!(rc, 0);

    assert_snapshots_equal(&path, &fixture_db_path());
}

#[test]
fn update_driver_dry_run_changes_nothing_and_leaves_no_temp_copy() {
    let path = copy_fixture("dry_run");
    let before = file_hash(&path);

    let mut opts = base_options(&path);
    opts.explicit_files = vec![synth_path("src/chain.cpp")];
    opts.dry_run = true;

    let mut summary = UpdateSummary::default();
    let rc = run_update(&opts, Some(&mut summary));
    assert_eq!(rc, 0);
    assert!(summary.dry_run);

    assert_eq!(file_hash(&path), before);

    let mut saw_temp_copy = false;
    let dir = Path::new(&path).parent().unwrap();
    let stem = Path::new(&path).file_stem().unwrap().to_string_lossy().into_owned();
    let prefix = format!("{}.incr-", stem);
    for entry in std::fs::read_dir(dir).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix) {
            saw_temp_copy = true;
        }
    }
    assert!(!saw_temp_copy);
}

#[test]
fn update_driver_in_place_dry_run_byte_identical_and_later_real_update_succeeds() {
    // Regression for the bug where --in-place --dry-run wrote
    // symhow_update_in_progress=1 on the REAL file before the dry-run early
    // return, and nothing ever cleared it -- bricking every later `symhow
    // update` (they'd all see the stale flag and refuse forever).
    let path = copy_fixture("inplace_dry_run");
    let before = file_hash(&path);

    let mut opts = base_options(&path);
    opts.explicit_files = vec![synth_path("src/chain.cpp")];
    opts.dry_run = true;
    opts.in_place = true;

    let mut summary = UpdateSummary::default();
    let rc = run_update(&opts, Some(&mut summary));
    assert_eq!(rc, 0);
    assert!(summary.dry_run);
    assert_eq!(file_hash(&path), before);

    {
        let db = WritableDb::open(&path, false).unwrap();
        assert!(get_meta(db.conn(), "symhow_update_in_progress").is_none());
    }

    // The real bug: a SUBSEQUENT real (non-dry-run) update must still
    // succeed -- it used to refuse with exit 2 forever after the dry run.
    let mut real_opts = opts.clone();
    real_opts.dry_run = false;
    let real_rc = run_update(&real_opts, None);
    assert_eq!(real_rc, 0);
}

#[test]
fn update_driver_dry_run_with_nothing_to_do_exits_5_and_changes_nothing() {
    // Regression for the bug where the "nothing to do" branch refreshed
    // symhow_repo_commit/symhow_dirty_paths/symhow_last_update_at even on
    // --dry-run. A --files path that's neither a known symhow_tu row nor a
    // CDB entry (here: the fixture's own CMakeLists.txt) classifies as
    // `unmatched`, which produces an empty clearSeed/deletedSeed/newTus --
    // i.e. genuinely "nothing to do" (exit 5).
    let path = copy_fixture("dry_run_nothing_to_do");
    let before = file_hash(&path);

    let mut opts = base_options(&path);
    opts.explicit_files = vec![synth_path("CMakeLists.txt")];
    opts.dry_run = true;

    let mut summary = UpdateSummary::default();
    let rc = run_update(&opts, Some(&mut summary));
    assert_eq!(rc, 5);
    assert!(summary.nothing_to_do);
    assert_eq!(file_hash(&path), before);
}

#[test]
fn update_driver_stale_lock_file_makes_run_update_refuse_with_exit_2() {
    let path = copy_fixture("lock_refuse");
    let lock_path = format!("{}.symhow-lock", path);
    let _ = std::fs::remove_file(&lock_path);
    std::fs::write(&lock_path, "held by a fake pid\n").unwrap();

    let mut opts = base_options(&path);
    opts.explicit_files = vec![synth_path("src/chain.cpp")];
    let rc = run_update(&opts, None);
    assert_eq!(rc, 2);

    // The lock file must still be there (run_update must never remove a
    // lock it didn't itself create).
    assert!(Path::new(&lock_path).exists());
    std::fs::remove_file(&lock_path).unwrap();

    // With the stale lock gone, the same request now succeeds.
    let rc2 = run_update(&opts, None);
    assert_eq!(rc2, 0);
    assert!(!Path::new(&lock_path).exists());
}

#[test]
fn update_driver_refuses_a_database_with_no_provenance() {
    let path = copy_fixture("no_provenance");
    {
        let mut db = WritableDb::open(&path, false).unwrap();
        db.begin().unwrap();
        symhow::db::meta::delete_meta(&mut db, "symhow_provenance_version").unwrap();
        db.commit().unwrap();
    }

    let mut opts = base_options(&path);
    opts.explicit_files = vec![synth_path("src/chain.cpp")];

    let rc = run_update(&opts, None);
    assert_eq!(rc, 2);
}

#[test]
fn update_driver_refuses_a_database_with_symhow_update_in_progress_set() {
    let path = copy_fixture("in_progress");
    {
        let mut db = WritableDb::open(&path, false).unwrap();
        db.begin().unwrap();
        set_meta(&mut db, "symhow_update_in_progress", "1").unwrap();
        db.commit().unwrap();
    }

    let mut opts = base_options(&path);
    opts.explicit_files = vec![synth_path("src/chain.cpp")];

    let rc = run_update(&opts, None);
    assert_eq!(rc, 2);
}
