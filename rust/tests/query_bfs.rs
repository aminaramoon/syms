//! Port of test/unit/test_bfs.cpp (fixture-dependent).

use symhow::db::enums::edge_kind;
use symhow::db::Database;
use symhow::query::{run_bfs, resolve_symbol, BfsParams, BfsResult, ResolveOutcome};

fn fixture_db_path() -> String {
    std::env::var("SYMHOW_FIXTURE_DB").unwrap_or_else(|_| "../cpp/build/fixture.srctrldb".to_string())
}

fn resolve_one(db: &Database, spec: &str) -> i64 {
    let rr = resolve_symbol(db, spec, None, None).unwrap();
    assert_eq!(rr.outcome, ResolveOutcome::Unique, "resolving '{}'", spec);
    rr.candidates[0].id
}

fn contains_node(r: &BfsResult, id: i64) -> bool {
    r.nodes.iter().any(|n| n.id == id)
}

fn non_seed_node_count(r: &BfsResult) -> usize {
    r.nodes.iter().filter(|n| n.depth > 0).count()
}

#[test]
fn callees_depth_limited_traversal_on_5_level_call_chain_saturates() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let level0 = resolve_one(&db, "level0");

    // level0 -> level1 -> level2 -> level3 -> level4 (4 hops, then no more).
    let expected_non_seed_counts = [1usize, 2, 3, 4, 4];
    for depth in 1..=5 {
        let p = BfsParams {
            seeds: vec![level0],
            edge_kind_mask: edge_kind::CALL,
            forward: true,
            depth,
            max_nodes: 0,
            node_kind_mask: 0,
        };
        let r = run_bfs(&db, &p).unwrap();
        assert_eq!(
            non_seed_node_count(&r),
            expected_non_seed_counts[(depth - 1) as usize],
            "depth={}",
            depth
        );
    }
}

#[test]
fn callees_node_counts_monotonically_non_decreasing_across_increasing_depth() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let level0 = resolve_one(&db, "level0");

    let mut previous = 0usize;
    for depth in 1..=5 {
        let p = BfsParams {
            seeds: vec![level0],
            edge_kind_mask: edge_kind::CALL,
            forward: true,
            depth,
            max_nodes: 0,
            node_kind_mask: 0,
        };
        let r = run_bfs(&db, &p).unwrap();
        assert!(non_seed_node_count(&r) >= previous);
        previous = non_seed_node_count(&r);
    }
}

#[test]
fn callers_terminates_on_ping_pong_mutual_recursion_and_includes_both() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let pong = resolve_one(&db, "pong");
    let ping = resolve_one(&db, "ping");

    let p = BfsParams {
        seeds: vec![pong],
        edge_kind_mask: edge_kind::CALL,
        forward: false, // callers: reverse
        depth: 0,       // unlimited -- must still terminate
        max_nodes: 0,
        node_kind_mask: 0,
    };

    let r = run_bfs(&db, &p).unwrap();
    assert!(!r.truncated);
    assert!(contains_node(&r, pong));
    assert!(contains_node(&r, ping));
}

#[test]
fn max_nodes_caps_the_result_and_sets_truncated() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let square = resolve_one(&db, "fixture::Square");

    let p = BfsParams {
        seeds: vec![square],
        edge_kind_mask: edge_kind::INHERITANCE,
        forward: true, // bases direction
        depth: 0,      // would otherwise reach Polygon, Colored, AND Shape
        max_nodes: 2,  // seed + exactly 1 more
        node_kind_mask: 0,
    };

    let r = run_bfs(&db, &p).unwrap();
    assert!(r.truncated);
    assert_eq!(r.nodes.len(), 2);
    // Both outgoing INHERITANCE edges from Square are still recorded even
    // though only one of their targets made it into the capped node list.
    assert_eq!(r.edges.len(), 2);
}

#[test]
fn inheritance_bases_direction_reaches_exactly_polygon_colored_shape() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let square = resolve_one(&db, "fixture::Square");
    let polygon = resolve_one(&db, "fixture::Polygon");
    let colored = resolve_one(&db, "fixture::Colored");
    let shape = resolve_one(&db, "fixture::Shape");

    let p = BfsParams {
        seeds: vec![square],
        edge_kind_mask: edge_kind::INHERITANCE,
        forward: true,
        depth: 0,
        max_nodes: 0,
        node_kind_mask: 0,
    };

    let r = run_bfs(&db, &p).unwrap();
    assert_eq!(non_seed_node_count(&r), 3);
    assert!(contains_node(&r, polygon));
    assert!(contains_node(&r, colored));
    assert!(contains_node(&r, shape));
}

#[test]
fn inheritance_derived_direction_reaches_exactly_polygon_colored_square() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let shape = resolve_one(&db, "fixture::Shape");
    let polygon = resolve_one(&db, "fixture::Polygon");
    let colored = resolve_one(&db, "fixture::Colored");
    let square = resolve_one(&db, "fixture::Square");

    let p = BfsParams {
        seeds: vec![shape],
        edge_kind_mask: edge_kind::INHERITANCE,
        forward: false,
        depth: 0,
        max_nodes: 0,
        node_kind_mask: 0,
    };

    let r = run_bfs(&db, &p).unwrap();
    assert_eq!(non_seed_node_count(&r), 3);
    assert!(contains_node(&r, polygon));
    assert!(contains_node(&r, colored));
    assert!(contains_node(&r, square));
}

#[test]
fn depth_1_bases_only_reaches_immediate_base_classes_not_shape() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let square = resolve_one(&db, "fixture::Square");
    let polygon = resolve_one(&db, "fixture::Polygon");
    let colored = resolve_one(&db, "fixture::Colored");
    let shape = resolve_one(&db, "fixture::Shape");

    let p = BfsParams {
        seeds: vec![square],
        edge_kind_mask: edge_kind::INHERITANCE,
        forward: true,
        depth: 1,
        max_nodes: 0,
        node_kind_mask: 0,
    };

    let r = run_bfs(&db, &p).unwrap();
    assert_eq!(non_seed_node_count(&r), 2);
    assert!(contains_node(&r, polygon));
    assert!(contains_node(&r, colored));
    assert!(!contains_node(&r, shape));
}