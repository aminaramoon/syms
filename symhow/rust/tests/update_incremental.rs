//! Port of test/unit/test_incremental_fixture.cpp: Phase 1
//! (incremental-update groundwork) assertions against the real synthetic
//! fixture database (build/fixture.srctrldb, via env SYMHOW_FIXTURE_DB --
//! the same env-var pattern as tests/query_fixture.rs).

use std::collections::BTreeSet;

use symhow::db::Database;
use symhow::index::provenance::{read_provenance, K_WALKER_VERSION};

fn fixture_db_path() -> String {
    std::env::var("SYMHOW_FIXTURE_DB").unwrap_or_else(|_| "../cpp/build/fixture.srctrldb".to_string())
}

fn scalar_i64(db: &Database, sql: &str) -> i64 {
    let mut value: i64 = -1;
    db.run(sql, [], |row| {
        value = row.get(0)?;
        Ok(())
    })
    .unwrap();
    value
}

#[test]
fn incremental_provenance_is_present_and_looks_right_on_freshly_indexed_fixture_db() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let prov = read_provenance(db.conn());
    assert!(prov.is_some());
    let prov = prov.unwrap();
    assert_eq!(prov.walker_version, K_WALKER_VERSION);
    assert!(!prov.cdb_path.is_empty());
    assert!(prov.cdb_path.starts_with('/')); // absolute
    assert!(!prov.indexed_at_utc.is_empty());
}

#[test]
fn incremental_symhow_tu_has_one_row_per_distinct_tu_path_each_with_16_hex_args_hash() {
    let db = Database::open(fixture_db_path(), 0).unwrap();

    // The synthetic fixture project has 7 distinct TU source files, but 8
    // CDB ENTRIES: templates.cpp is compiled twice, by two targets with
    // different flags, exercising the duplicate-CDB-entry path that
    // symhow_tu -- keyed by PATH, not by (path, entry) -- has to collapse
    // down to one row. symhow_tu row count tracks distinct paths (7), not
    // CDB entries (8).
    assert_eq!(scalar_i64(&db, "SELECT count(*) FROM symhow_tu"), 7);

    let mut rows = 0i64;
    db.run("SELECT args_hash FROM symhow_tu", [], |row| {
        let hash: Option<String> = row.get(0)?;
        let hash = hash.unwrap_or_default();
        assert_eq!(hash.len(), 16);
        assert!(hash.bytes().all(|c| c.is_ascii_hexdigit()));
        rows += 1;
        Ok(())
    })
    .unwrap();
    assert!(rows > 0);
}

#[test]
fn incremental_symhow_tu_entry_count_reflects_real_cdb_entry_count_per_path() {
    // Regression coverage for the duplicate-CDB-entry fix: templates.cpp is
    // compiled twice, so its symhow_tu row must carry entry_count=2; every
    // other (non-duplicated) path must carry the legacy-compatible default
    // of 1.
    let db = Database::open(fixture_db_path(), 0).unwrap();

    let mut templates_entry_count: i64 = -1;
    let mut found_templates = false;
    db.run("SELECT path, entry_count FROM symhow_tu", [], |row| {
        let path: Option<String> = row.get(0)?;
        let path = path.unwrap_or_default();
        let entry_count: i64 = row.get(1)?;
        if path.ends_with("templates.cpp") {
            templates_entry_count = entry_count;
            found_templates = true;
        } else {
            assert_eq!(entry_count, 1);
        }
        Ok(())
    })
    .unwrap();
    assert!(found_templates);
    assert_eq!(templates_entry_count, 2);
}

#[test]
fn incremental_every_symhow_tu_row_from_a_fresh_index_has_status_ok() {
    // Phase 4: symhow_tu.status is the precise per-TU outcome tracked instead
    // of the old (Phase 3) "no outgoing INCLUDE edge" heuristic. The
    // synthetic fixture is a clean, fully-parseable corpus, so every row
    // must be "ok".
    let db = Database::open(fixture_db_path(), 0).unwrap();

    assert_eq!(scalar_i64(&db, "SELECT count(*) FROM symhow_tu"), 7);
    assert_eq!(scalar_i64(&db, "SELECT count(*) FROM symhow_tu WHERE status != 'ok'"), 0);
}

#[test]
fn incremental_symhow_symbol_def_is_non_empty_and_level0_declaration_points_at_chain_cpp() {
    let db = Database::open(fixture_db_path(), 0).unwrap();

    assert!(scalar_i64(&db, "SELECT count(*) FROM symhow_symbol_def") > 0);

    let mut files_for_level0: BTreeSet<String> = BTreeSet::new();
    db.run(
        "SELECT f.path FROM node n \
         JOIN symhow_symbol_def sd ON sd.node_id = n.id \
         JOIN file f ON f.id = sd.file_node_id \
         WHERE n.serialized_name LIKE '%level0%'",
        [],
        |row| {
            let path: Option<String> = row.get(0)?;
            files_for_level0.insert(path.unwrap_or_default());
            Ok(())
        },
    )
    .unwrap();
    assert!(!files_for_level0.is_empty());
    assert!(files_for_level0.iter().any(|path| path.ends_with("chain.cpp")));
}

#[test]
fn incremental_every_non_member_edge_has_at_least_one_occurrence() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    assert_eq!(
        scalar_i64(
            &db,
            "SELECT COUNT(*) FROM edge e WHERE e.type<>1 AND NOT EXISTS \
             (SELECT 1 FROM occurrence o WHERE o.element_id = e.id)"
        ),
        0
    );
}

#[test]
fn incremental_no_source_location_references_a_file_node_id_missing_from_node() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    assert_eq!(
        scalar_i64(
            &db,
            "SELECT COUNT(*) FROM source_location sl WHERE sl.file_node_id NOT IN (SELECT id FROM node)"
        ),
        0
    );
}

// --- node.type upgrade-only regression coverage (BLOCKER finding #1) -------
//
// These three cases pin the fix: no symbol-backed node should be left at
// type=1 (UNKNOWN), the only type=1 node left should be the vendored
// writer's single shared "unsolved symbol" node, and at least one node
// should have been upgraded to type=2 (TYPE) -- template type parameters
// and the like.

#[test]
fn incremental_no_symbol_backed_node_is_left_at_node_type_unknown() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    assert_eq!(scalar_i64(&db, "SELECT COUNT(*) FROM node n JOIN symbol s ON s.id = n.id WHERE n.type = 1"), 0);
}

#[test]
fn incremental_exactly_one_node_type_unknown_remains_the_shared_unsolved_symbol_node() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    assert_eq!(scalar_i64(&db, "SELECT COUNT(*) FROM node WHERE type = 1"), 1);
}

#[test]
fn incremental_at_least_one_node_was_upgraded_to_node_type_type() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    assert!(scalar_i64(&db, "SELECT COUNT(*) FROM node WHERE type = 2") > 0);
}

// --- symhow_tu.file_node_id must always resolve (path-normalization, #2) ------

#[test]
fn incremental_every_symhow_tu_row_has_a_resolved_file_node_id() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    assert_eq!(scalar_i64(&db, "SELECT COUNT(*) FROM symhow_tu WHERE file_node_id IS NULL"), 0);
}

// --- definition_kind upgrade-only, and a header-decl/cpp-def symbol (#5) --

#[test]
fn incremental_every_symbol_row_definition_kind_is_explicit_2_in_this_fixture() {
    // The synthetic fixture only exercises symbols that get an explicit
    // definition somewhere -- definition_kind should never regress below
    // Explicit (2) for any symbol row visited by this fixture's TUs.
    let db = Database::open(fixture_db_path(), 0).unwrap();
    assert_eq!(scalar_i64(&db, "SELECT COUNT(*) FROM symbol WHERE definition_kind < 2"), 0);
}

#[test]
fn incremental_header_declared_cpp_defined_function_has_explicit_kind_and_two_def_rows() {
    // fixture::squarePerimeter is declared in include/shapes.hpp and
    // defined out-of-line in src/ns.cpp -- this pins down that (a) the
    // out-of-line definition wins definition_kind = Explicit (2), not just
    // "declared", and (b) symhow_symbol_def has one row per file the symbol
    // was declared/defined in (header AND cpp), not just the last one
    // processed.
    let db = Database::open(fixture_db_path(), 0).unwrap();

    let mut definition_kind: i64 = -1;
    let mut found = false;
    db.run(
        "SELECT s.definition_kind FROM node n JOIN symbol s ON s.id = n.id \
         WHERE n.serialized_name LIKE '%squarePerimeter%'",
        [],
        |row| {
            definition_kind = row.get(0)?;
            found = true;
            Ok(())
        },
    )
    .unwrap();
    assert!(found);
    assert_eq!(definition_kind, 2); // Explicit

    assert_eq!(
        scalar_i64(
            &db,
            "SELECT COUNT(*) FROM symhow_symbol_def sd \
             JOIN node n ON n.id = sd.node_id \
             WHERE n.serialized_name LIKE '%squarePerimeter%'"
        ),
        2
    );

    let mut decl_files: BTreeSet<String> = BTreeSet::new();
    db.run(
        "SELECT f.path FROM symhow_symbol_def sd \
         JOIN node n ON n.id = sd.node_id \
         JOIN file f ON f.id = sd.file_node_id \
         WHERE n.serialized_name LIKE '%squarePerimeter%'",
        [],
        |row| {
            let path: Option<String> = row.get(0)?;
            decl_files.insert(path.unwrap_or_default());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(decl_files.len(), 2);
    assert!(decl_files.iter().any(|path| path.ends_with("shapes.hpp")));
    assert!(decl_files.iter().any(|path| path.ends_with("ns.cpp")));
}

// --- Phase 5: every GC rule Invalidator applies must be a provable no-op on
// a freshly indexed database (one that was never invalidated). Each test
// below runs the EXACT predicate the corresponding rule in the invalidator
// deletes by, and requires it to match 0 rows.

#[test]
fn incremental_a_fresh_index_symhow_touched_covers_every_node_gc_guard_is_total() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    assert_eq!(
        scalar_i64(&db, "SELECT COUNT(*) FROM node n WHERE n.id NOT IN (SELECT node_id FROM symhow_touched)"),
        0
    );
}

#[test]
fn incremental_invalidators_dangling_edge_rule_matches_0_rows_on_a_fresh_index() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    assert_eq!(
        scalar_i64(
            &db,
            "SELECT COUNT(*) FROM edge e WHERE e.type<>1 AND e.source_node_id NOT IN (SELECT id FROM file) \
             AND e.source_node_id NOT IN (SELECT node_id FROM symhow_touched) \
             AND NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.element_id=e.source_node_id)"
        ),
        0
    );
}

#[test]
fn incremental_invalidators_dead_node_rule_matches_0_rows_on_a_fresh_index() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    assert_eq!(
        scalar_i64(
            &db,
            "SELECT COUNT(*) FROM node n WHERE n.id NOT IN (SELECT id FROM file) \
             AND n.id NOT IN (SELECT node_id FROM symhow_touched) \
             AND NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.element_id=n.id) \
             AND NOT EXISTS (SELECT 1 FROM edge e WHERE e.type<>1 AND (e.source_node_id=n.id OR e.target_node_id=n.id)) \
             AND NOT EXISTS (SELECT 1 FROM edge e WHERE e.type=1 AND e.source_node_id=n.id)"
        ),
        0
    );
}

#[test]
fn incremental_no_non_error_occurrence_location_lands_in_a_non_indexed_file() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    // A fact is only ever emitted if ITS OWN range resolves to an indexed
    // file. `source_location.type = 6` is the INDEXER_ERROR kind, which
    // legitimately attaches to the TU's own main file even when the
    // diagnostic came from elsewhere.
    assert_eq!(
        scalar_i64(
            &db,
            "SELECT COUNT(*) FROM source_location l JOIN file f ON f.id = l.file_node_id \
             WHERE f.indexed = 0 AND l.type <> 6"
        ),
        0
    );
}
