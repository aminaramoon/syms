//! Port of test/unit/test_snapshot.cpp (fixture-based cases; the CLI
//! `dbdiff` smoke tests and the notes-cache tests are TODO(parity) until the
//! CLI/notes layers land).

use symhow::update::snapshot::{diff_snapshots, take_snapshot, Snapshot, SnapshotOptions};

fn fixture_db_path() -> String {
    std::env::var("SYMHOW_FIXTURE_DB").unwrap_or_else(|_| "../cpp/build/fixture.srctrldb".to_string())
}

fn cuda_fixture_db_path() -> String {
    std::env::var("SYMHOW_CUDA_FIXTURE_DB").unwrap_or_else(|_| "../cpp/build/cuda_fixture.srctrldb".to_string())
}

#[test]
fn self_diff_is_empty_reflexivity() {
    let a = take_snapshot(&fixture_db_path(), &SnapshotOptions::default()).unwrap();
    let b = take_snapshot(&fixture_db_path(), &SnapshotOptions::default()).unwrap();
    let diff = diff_snapshots(&a, &b, &[]);
    assert!(diff.only_in_a.is_empty());
    assert!(diff.only_in_b.is_empty());
    assert!(!a.nodes.is_empty()); // sanity: the fixture actually has content
}

#[test]
fn fixture_vs_cuda_fixture_is_non_empty_with_node_entries() {
    let a = take_snapshot(&fixture_db_path(), &SnapshotOptions::default()).unwrap();
    let b = take_snapshot(&cuda_fixture_db_path(), &SnapshotOptions::default()).unwrap();
    let diff = diff_snapshots(&a, &b, &[]);

    assert!(!diff.only_in_a.is_empty());
    assert!(!diff.only_in_b.is_empty());
    assert!(diff.only_in_a.iter().any(|s| s.starts_with("node|")));
}

#[test]
fn diff_is_symmetric_when_args_swap() {
    let a = take_snapshot(&fixture_db_path(), &SnapshotOptions::default()).unwrap();
    let b = take_snapshot(&cuda_fixture_db_path(), &SnapshotOptions::default()).unwrap();
    let ab = diff_snapshots(&a, &b, &[]);
    let ba = diff_snapshots(&b, &a, &[]);
    assert_eq!(ab.only_in_a.len(), ba.only_in_b.len());
    assert_eq!(ab.only_in_b.len(), ba.only_in_a.len());
}

#[test]
fn reports_multiset_count_mismatch_not_silent_collapse() {
    let mut a = Snapshot::default();
    a.local_symbols.insert("dup".to_string(), 2);
    a.local_symbols.insert("only_a".to_string(), 1);

    let mut b = Snapshot::default();
    b.local_symbols.insert("dup".to_string(), 1);
    b.local_symbols.insert("only_b".to_string(), 1);

    let diff = diff_snapshots(&a, &b, &[]);

    assert!(diff.only_in_a.iter().any(|s| s == "local_symbol|dup (x2 vs x1)"));
    assert!(diff.only_in_b.iter().any(|s| s == "local_symbol|dup (x1 vs x2)"));
    assert!(diff.only_in_a.iter().any(|s| s == "local_symbol|only_a"));
    assert!(diff.only_in_b.iter().any(|s| s == "local_symbol|only_b"));
}

#[test]
fn duplicated_keys_with_equal_counts_is_empty() {
    let mut a = Snapshot::default();
    a.nodes.insert("dup|4096".to_string(), 3);
    let diff = diff_snapshots(&a, &a, &[]);
    assert!(diff.only_in_a.is_empty());
    assert!(diff.only_in_b.is_empty());
}

#[test]
fn only_filter_restricts_categories() {
    let a = take_snapshot(&fixture_db_path(), &SnapshotOptions::default()).unwrap();
    let b = take_snapshot(&cuda_fixture_db_path(), &SnapshotOptions::default()).unwrap();
    let diff = diff_snapshots(&a, &b, &["file".to_string()]);
    assert!(diff.only_in_a.iter().all(|s| s.starts_with("file|")));
    assert!(diff.only_in_b.iter().all(|s| s.starts_with("file|")));
}

#[test]
fn ignore_shared_header_locations_drops_only_shared_header_locations() {
    let full = take_snapshot(&fixture_db_path(), &SnapshotOptions::default()).unwrap();

    let filtered = take_snapshot(
        &fixture_db_path(),
        &SnapshotOptions { ignore_shared_header_locations: true },
    )
    .unwrap();

    // Every OTHER category is untouched.
    assert_eq!(filtered.nodes, full.nodes);
    assert_eq!(filtered.edges, full.edges);
    assert_eq!(filtered.symbols, full.symbols);
    assert_eq!(filtered.files, full.files);
    assert_eq!(filtered.errors, full.errors);
    assert_eq!(filtered.local_symbols, full.local_symbols);
    assert_eq!(filtered.symbol_defs, full.symbol_defs);
    assert_eq!(filtered.tus, full.tus);

    // The synthetic fixture has headers included by more than one TU
    // (shapes.hpp / ns.hpp), so something must have been dropped, and what
    // remains must be a strict subset of the unfiltered snapshot.
    assert!(filtered.locations.len() < full.locations.len());
    for (key, count) in &filtered.locations {
        assert_eq!(full.locations.get(key), Some(count));
    }

    // Self-diff under the same option is still empty (reflexivity holds).
    let self_diff = diff_snapshots(&filtered, &filtered, &[]);
    assert!(self_diff.only_in_a.is_empty());
    assert!(self_diff.only_in_b.is_empty());
}

// TODO(parity): port the `symhow dbdiff` CLI smoke tests (equal exit 0 /
// different exit 3 / missing exit 4 / --json envelope) and the notes-cache
// dbdiff test once the CLI and notes layers are ported.