//! Port of test/unit/test_invalidator.cpp — exercises
//! `compute_clear_set`/`invalidate`/`sweep_orphans` against private copies
//! of the synthetic fixture database.

use std::collections::HashSet;

use symhow::common::paths::normalize;
use symhow::db::Database;
use symhow::db::WritableDb;
use symhow::update::invalidator::{compute_clear_set, invalidate, sweep_orphans};

fn fixture_db_path() -> String {
    std::env::var("SYMHOW_FIXTURE_DB").unwrap_or_else(|_| "../cpp/build/fixture.srctrldb".to_string())
}

fn copy_fixture(tag: &str) -> String {
    let path = std::env::temp_dir().join(format!("symhow_test_invalidator_{}.srctrldb", tag));
    let _ = std::fs::remove_file(&path);
    std::fs::copy(fixture_db_path(), &path).unwrap();
    path.to_string_lossy().into_owned()
}

fn all_tu_paths(db_path: &str) -> Vec<String> {
    let db = Database::open(db_path, 0).unwrap();
    let mut paths = Vec::new();
    db.run("SELECT path FROM symhow_tu", [], |row| {
        if let Some(p) = row.get::<_, Option<String>>(0)? {
            paths.push(p);
        }
        Ok(())
    })
    .unwrap();
    paths
}

/// Resolves a fixture source path (relative to test/fixtures/synthetic) to
/// its exact `file.path` spelling, matched via normalization.
fn resolve_fixture_path(db_path: &str, relative_to_synthetic: &str) -> String {
    let synthetic = std::path::Path::new("../cpp/test/fixtures/synthetic").join(relative_to_synthetic);
    let wanted = normalize(&synthetic.to_string_lossy());

    let db = Database::open(db_path, 0).unwrap();
    let mut found = String::new();
    db.run("SELECT path FROM file", [], |row| {
        if !found.is_empty() {
            return Ok(());
        }
        if let Some(p) = row.get::<_, Option<String>>(0)? {
            if normalize(&p) == wanted {
                found = p;
            }
        }
        Ok(())
    })
    .unwrap();
    assert!(!found.is_empty(), "fixture path not found for {}", relative_to_synthetic);
    found
}

fn base_names(paths: &[String]) -> HashSet<String> {
    paths
        .iter()
        .map(|p| {
            std::path::Path::new(p)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn compute_clear_set_chain_cpp_is_exactly_that_one_file() {
    let path = copy_fixture("chain_compute");
    let tu_paths = all_tu_paths(&path);
    let chain_path = resolve_fixture_path(&path, "src/chain.cpp");

    let mut db = WritableDb::open(&path, true).unwrap();
    let plan = compute_clear_set(&mut db, &[chain_path], &[], &tu_paths).unwrap();

    assert_eq!(plan.cleared_file_ids.len(), 1);
    assert_eq!(plan.cleared_paths.len(), 1);
    assert_eq!(plan.include_closure_added, 0);
    assert_eq!(plan.headers_pruned, 0);
    assert_eq!(base_names(&plan.cleared_paths), HashSet::from(["chain.cpp".to_string()]));
}

#[test]
fn compute_clear_set_shapes_hpp_closes_over_exactly_its_includers() {
    let path = copy_fixture("shapes_compute");
    let tu_paths = all_tu_paths(&path);
    let shapes_path = resolve_fixture_path(&path, "include/shapes.hpp");

    let mut db = WritableDb::open(&path, true).unwrap();
    let plan = compute_clear_set(&mut db, &[shapes_path], &[], &tu_paths).unwrap();

    let names = base_names(&plan.cleared_paths);
    assert_eq!(names.get("shapes.hpp"), Some(&"shapes.hpp".to_string()));
    assert_eq!(names.get("ns.cpp"), Some(&"ns.cpp".to_string()));
    assert_eq!(names.get("overloads.cpp"), Some(&"overloads.cpp".to_string()));
    assert_eq!(plan.include_closure_added, 2); // exactly the two TUs that include shapes.hpp

    // Header pruning sweeps in headers included ONLY by the cleared TUs;
    // it must NOT sweep in any TU that doesn't include shapes.hpp at all.
    assert!(!names.contains("chain.cpp"));
    assert!(!names.contains("templates.cpp"));
    assert!(!names.contains("tu_a.cpp"));
    assert!(!names.contains("tu_b.cpp"));
}

#[test]
fn invalidate_chain_plan_clears_facts_and_stays_fk_consistent() {
    let path = copy_fixture("chain_invalidate");
    let tu_paths = all_tu_paths(&path);
    let chain_path = resolve_fixture_path(&path, "src/chain.cpp");

    // `Shape` lives in shapes.hpp (untouched by this plan) -- looked up by
    // serialized_name before invalidation so post-invalidation existence
    // can be checked by id.
    let mut shape_node_id_before: i64 = -1;
    {
        let before = Database::open(&path, 0).unwrap();
        before
            .run(
                "SELECT id FROM node WHERE serialized_name LIKE '%Shape%' AND serialized_name NOT LIKE '%Shapes%' LIMIT 1",
                [],
                |row| {
                    shape_node_id_before = row.get(0)?;
                    Ok(())
                },
            )
            .unwrap();
    }
    assert!(shape_node_id_before >= 0);

    let gc;
    {
        let mut db = WritableDb::open(&path, true).unwrap();
        let plan = compute_clear_set(&mut db, &[chain_path.clone()], &[], &tu_paths).unwrap();
        assert_eq!(plan.cleared_file_ids.len(), 1);
        gc = invalidate(&mut db, &plan).unwrap();
    }

    assert_eq!(gc.files, 1);

    let after = Database::open(&path, 0).unwrap();

    // No source_location left pointing at the cleared file.
    let mut remaining_locations: i64 = -1;
    after
        .run(
            "SELECT COUNT(*) FROM source_location l JOIN file f ON f.id = l.file_node_id WHERE f.path = ?1",
            [&chain_path],
            |row| {
                remaining_locations = row.get(0)?;
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(remaining_locations, 0);

    // No dead nodes left (occurrence-less, edge-less, member-less, non-file).
    let mut dead_nodes: i64 = -1;
    after
        .run(
            "SELECT COUNT(*) FROM node n WHERE n.id NOT IN (SELECT id FROM file) \
             AND NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.element_id = n.id) \
             AND NOT EXISTS (SELECT 1 FROM edge e WHERE e.type <> 1 AND (e.source_node_id = n.id OR e.target_node_id = n.id)) \
             AND NOT EXISTS (SELECT 1 FROM edge e WHERE e.type = 1 AND e.source_node_id = n.id)",
            [],
            |row| {
                dead_nodes = row.get(0)?;
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(dead_nodes, 0);

    // FK consistency.
    let mut fk_violations: i64 = 0;
    after
        .run("PRAGMA foreign_key_check", [], |_row| {
            fk_violations += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(fk_violations, 0);

    // A node defined only in an untouched file (shapes.hpp's Shape) must
    // still exist, by the SAME id.
    let mut shape_node_id_after: i64 = -1;
    after
        .run("SELECT id FROM node WHERE id = ?1", [shape_node_id_before], |row| {
            shape_node_id_after = row.get(0)?;
            Ok(())
        })
        .unwrap();
    assert_eq!(shape_node_id_after, shape_node_id_before);
}

#[test]
fn invalidate_shapes_plan_leaves_zero_dangling_rows() {
    let path = copy_fixture("shapes_dangling");
    let tu_paths = all_tu_paths(&path);
    let shapes_path = resolve_fixture_path(&path, "include/shapes.hpp");

    {
        let mut db = WritableDb::open(&path, true).unwrap();
        let plan = compute_clear_set(&mut db, &[shapes_path], &[], &tu_paths).unwrap();
        assert!(plan.cleared_file_ids.len() > 1);
        invalidate(&mut db, &plan).unwrap();
    }

    let after = Database::open(&path, 0).unwrap();

    let mut dangling_symbol_def: i64 = -1;
    after
        .run(
            "SELECT COUNT(*) FROM symhow_symbol_def WHERE node_id NOT IN (SELECT id FROM node) OR file_node_id NOT IN (SELECT id FROM file)",
            [],
            |row| {
                dangling_symbol_def = row.get(0)?;
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(dangling_symbol_def, 0);

    let mut dangling_tu: i64 = -1;
    after
        .run(
            "SELECT COUNT(*) FROM symhow_tu WHERE file_node_id IS NOT NULL AND file_node_id NOT IN (SELECT id FROM file)",
            [],
            |row| {
                dangling_tu = row.get(0)?;
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(dangling_tu, 0);
}

#[test]
fn sweep_orphans_on_pristine_fixture_copy_is_a_no_op() {
    let path = copy_fixture("sweep_noop");
    let mut db = WritableDb::open(&path, true).unwrap();
    let gc = sweep_orphans(&mut db).unwrap();
    assert_eq!(gc.nodes, 0);
    assert_eq!(gc.local_symbols, 0);
    assert_eq!(gc.errors, 0);
    assert_eq!(gc.non_indexed_files, 0);
    assert_eq!(gc.orphan_elements, 0);
}