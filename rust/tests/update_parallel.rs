//! Port of test/unit/test_parallel_fixture.cpp: `--parallel`/`-j`
//! equivalence tests -- indexes the real synthetic fixture CDB (env
//! SYMHOW_FIXTURE_CDB, falling back to ../build/fixture_build/
//! compile_commands.json) through run_index() with different `parallel`
//! values into separate temp databases, and asserts the RESULT is
//! identical regardless of N -- both at the logical-snapshot level
//! (diff_snapshots, id-independent) and, crucially, at the raw sqlite
//! `node` table level INCLUDING ids: the design's determinism guarantee is
//! that the reorder buffer makes the consumer see (and therefore assign
//! ids to) TUs in the exact same CDB order no matter how many worker
//! threads raced to produce them.

use symhow::db::Database;
use symhow::index::index_driver::{run_index, IndexRunOptions};
use symhow::update::snapshot::{diff_snapshots, take_snapshot, Snapshot, SnapshotOptions};

fn fixture_cdb_path() -> String {
    std::env::var("SYMHOW_FIXTURE_CDB").unwrap_or_else(|_| "../build/fixture_build/compile_commands.json".to_string())
}

fn test_output_dir(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("symhow_parallel_test_{}_{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.to_string_lossy().into_owned()
}

// Runs a full `symhow index` (via the real run_index(), never a shell-out)
// of the fixture CDB into `output_path` with the given `parallel` value.
// Returns the process exit code (expected 0: the fixture always produces
// content).
fn index_fixture(output_path: &str, parallel: i32, commit_every: i32) -> i32 {
    let mut opts = IndexRunOptions::default();
    opts.compile_commands_path = fixture_cdb_path();
    opts.output_path = output_path.to_string();
    opts.force = true;
    opts.quiet = true;
    opts.super_quiet = true;
    opts.parallel = parallel;
    opts.commit_every = commit_every;
    opts.symhow_version = "test-version".to_string();
    run_index(&opts, None)
}

// "id, serialized_name" pairs from `node`, ordered by id -- the strictest
// possible check: not just that the same logical nodes exist (that's what
// take_snapshot/diff_snapshots already proves, id-independently) but that
// they were assigned the SAME ids in the SAME order.
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

fn snapshot_of(path: &str) -> Snapshot {
    take_snapshot(path, &SnapshotOptions::default()).unwrap()
}

fn assert_snapshots_equal(a: &str, b: &str) {
    let diff = diff_snapshots(&snapshot_of(a), &snapshot_of(b), &[]);
    assert!(diff.only_in_a.is_empty(), "only in {}: {:?}", a, diff.only_in_a);
    assert!(diff.only_in_b.is_empty(), "only in {}: {:?}", b, diff.only_in_b);
}

#[test]
fn parallel_1_vs_4_produce_an_identical_database_snapshot_and_node_ids() {
    let dir = test_output_dir("p1_vs_p4");
    let db_a = format!("{}/parallel_test_p1.srctrldb", dir);
    let db_b = format!("{}/parallel_test_p4.srctrldb", dir);

    assert_eq!(index_fixture(&db_a, 1, 0), 0);
    assert_eq!(index_fixture(&db_b, 4, 0), 0);

    assert_snapshots_equal(&db_a, &db_b);

    // The determinism guarantee: node ids themselves (not just the logical
    // content) must match exactly, in the exact same order.
    let nodes_a = node_ids_and_names(&db_a);
    let nodes_b = node_ids_and_names(&db_b);
    assert!(!nodes_a.is_empty());
    assert_eq!(nodes_a, nodes_b);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn parallel_0_hardware_concurrency_also_matches_the_sequential_result() {
    let dir = test_output_dir("p0");
    let db_a = format!("{}/parallel_test_p1b.srctrldb", dir);
    let db_c = format!("{}/parallel_test_p0.srctrldb", dir);

    assert_eq!(index_fixture(&db_a, 1, 0), 0);
    assert_eq!(index_fixture(&db_c, 0, 0), 0);

    assert_snapshots_equal(&db_a, &db_c);

    assert_eq!(node_ids_and_names(&db_a), node_ids_and_names(&db_c));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn parallel_4_with_commit_every_3_still_matches_the_sequential_single_transaction_result() {
    // Phase 8: checkpointing (commit_every > 0) must not interact badly with
    // the parallel reorder buffer -- checkpoints only ever happen on the
    // single consumer thread, after a TU is consumed in strict CDB order,
    // so a chunk's symhow_tu rows are always a contiguous CDB prefix
    // regardless of how many worker threads raced to produce them.
    let dir = test_output_dir("p4_ce3");
    let db_a = format!("{}/parallel_test_p1_ce0.srctrldb", dir);
    let db_b = format!("{}/parallel_test_p4_ce3.srctrldb", dir);

    assert_eq!(index_fixture(&db_a, 1, 0), 0);
    assert_eq!(index_fixture(&db_b, 4, 3), 0);

    assert_snapshots_equal(&db_a, &db_b);

    assert_eq!(node_ids_and_names(&db_a), node_ids_and_names(&db_b));

    let _ = std::fs::remove_dir_all(&dir);
}
