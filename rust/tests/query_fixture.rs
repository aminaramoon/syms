//! Port of test/unit/test_query_fixture.cpp (fixture-dependent).

use std::collections::BTreeSet;

use symhow::db::enums::{edge_kind, location_kind, node_kind};
use symhow::db::name_decode::decode_serialized_name;
use symhow::db::Database;
use symhow::query::{run_bfs, resolve_symbol, BfsParams, BfsResult, QueryEngine, ResolveOutcome};

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

// bases()/derived()/etc. always include the seed(s) themselves at depth 0, so
// tests that check "exactly {A, B, C}" compare against the non-seed nodes.
fn non_seed_ids(r: &BfsResult) -> BTreeSet<i64> {
    r.nodes.iter().filter(|n| n.depth > 0).map(|n| n.id).collect()
}

#[test]
fn bases_square_reaches_exactly_polygon_colored_shape_at_depth_0_and_polygon_colored_at_depth_1() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let engine = QueryEngine::new(&db).unwrap();

    let square = resolve_one(&db, "fixture::Square");
    let polygon = resolve_one(&db, "fixture::Polygon");
    let colored = resolve_one(&db, "fixture::Colored");
    let shape = resolve_one(&db, "fixture::Shape");

    let depth0 = non_seed_ids(&engine.bases(&[square], 0, 0, 0).unwrap());
    assert_eq!(depth0, BTreeSet::from([polygon, colored, shape]));

    let depth1 = non_seed_ids(&engine.bases(&[square], 1, 0, 0).unwrap());
    assert_eq!(depth1, BTreeSet::from([polygon, colored]));
}

#[test]
fn derived_shape_reaches_exactly_polygon_colored_square_at_depth_0() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let engine = QueryEngine::new(&db).unwrap();

    let shape = resolve_one(&db, "fixture::Shape");
    let polygon = resolve_one(&db, "fixture::Polygon");
    let colored = resolve_one(&db, "fixture::Colored");
    let square = resolve_one(&db, "fixture::Square");

    let depth0 = non_seed_ids(&engine.derived(&[shape], 0, 0, 0).unwrap());
    assert_eq!(depth0, BTreeSet::from([polygon, colored, square]));
}

#[test]
fn override_edges_shape_area_transitively_overridden_by_all_3_subclass_methods() {
    let db = Database::open(fixture_db_path(), 0).unwrap();

    let shape_area = resolve_one(&db, "fixture::Shape::area");
    let polygon_area = resolve_one(&db, "fixture::Polygon::area");
    let colored_area = resolve_one(&db, "fixture::Colored::area");
    let square_area = resolve_one(&db, "fixture::Square::area");

    let p = BfsParams {
        seeds: vec![shape_area],
        edge_kind_mask: edge_kind::OVERRIDE,
        forward: false, // overriddenBy direction
        depth: 0,       // unlimited, unlike the depth=1 QueryEngine::overridden_by() verb
        max_nodes: 0,
        node_kind_mask: 0,
    };

    let r = run_bfs(&db, &p).unwrap();
    assert!(r.edges.len() >= 3);
    assert!(contains_node(&r, polygon_area));
    assert!(contains_node(&r, colored_area));
    assert!(contains_node(&r, square_area));

    // Exactly 2 OVERRIDE edges have Shape::area as their DIRECT target.
    let direct_count = r.edges.iter().filter(|e| e.target == shape_area).count();
    assert_eq!(direct_count, 2);

    // depth=1 QueryEngine::overridden_by() only sees the direct overriders.
    let engine = QueryEngine::new(&db).unwrap();
    let direct = non_seed_ids(&engine.overridden_by(&[shape_area], 0).unwrap());
    assert_eq!(direct, BTreeSet::from([polygon_area, colored_area]));
}

#[test]
fn callees_level0_monotonically_non_decreasing_and_saturates_by_depth_4() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let engine = QueryEngine::new(&db).unwrap();
    let level0 = resolve_one(&db, "level0");

    let mut previous = 0usize;
    for depth in 1..=5 {
        let ids = non_seed_ids(&engine.callees(&[level0], depth, 0, 0).unwrap());
        assert!(ids.len() >= previous);
        previous = ids.len();
    }
    assert_eq!(previous, 4); // level1, level2, level3, level4
}

#[test]
fn callers_pong_terminates_on_mutual_recursion_and_includes_ping() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let engine = QueryEngine::new(&db).unwrap();
    let pong = resolve_one(&db, "pong");
    let ping = resolve_one(&db, "ping");

    let r = engine.callers(&[pong], 0, 0, 0).unwrap();
    assert!(!r.truncated);
    assert!(contains_node(&r, pong));
    assert!(contains_node(&r, ping));
}

#[test]
fn header_included_by_two_tus_dedups_to_exactly_one_node_per_symbol() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    // shapes.hpp is #included by both ns.cpp and overloads.cpp; if dedup by
    // USR-derived SymbolKey ever regressed, Square would show up as (at
    // least) 2 separate CLASS nodes with the exact same serialized_name.
    let square_serialized = "::\tmfixture\ts\tp\tnSquare\ts\tp";
    let mut count = 0i64;
    db.run(
        "SELECT COUNT(*) FROM node WHERE type = ? AND serialized_name = ?",
        rusqlite::params![node_kind::CLASS, square_serialized],
        |row| {
            count = row.get::<_, i64>(0)?;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn resolve_symbol_on_tu_a_tu_b_usr_collision_qualified_names_resolves_4_distinct_ids() {
    let db = Database::open(fixture_db_path(), 0).unwrap();

    // The two TUs each define their own helper()/main(); NameBuilder appends
    // a USR-derived hash suffix to disambiguate the colliding qualified
    // names. The suffix bytes are clang-version specific, so they're resolved
    // from the fixture DB rather than hardcoded.
    let bases = [
        ("helper", "tu_a.cpp"),
        ("helper", "tu_b.cpp"),
        ("main", "tu_a.cpp"),
        ("main", "tu_b.cpp"),
    ];

    let mut ids: BTreeSet<i64> = BTreeSet::new();
    for (base, tu) in bases {
        let mut spec = String::new();
        let mut rows = 0i64;
        let pattern = format!("%m{}%() [{}#%", base, tu);
        db.run(
            "SELECT serialized_name FROM node WHERE serialized_name LIKE ?",
            [pattern.as_str()],
            |row| {
                if spec.is_empty() {
                    let serialized = row.get::<_, Option<String>>(0)?.unwrap_or_default();
                    spec = decode_serialized_name(&serialized).qualified_name();
                }
                rows += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(rows, 1, "looking for '{}() [{}#...]'", base, tu);
        assert!(!spec.is_empty());

        let rr = resolve_symbol(&db, &spec, None, None).unwrap();
        assert_eq!(rr.outcome, ResolveOutcome::Unique, "resolving '{}'", spec);
        ids.insert(rr.candidates[0].id);
    }
    assert_eq!(ids.len(), 4);
}

#[test]
fn search_sqr_kind_class_ranks_square_first() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let engine = QueryEngine::new(&db).unwrap();

    let class_only = [node_kind::CLASS];
    let hits = engine.search("sqr", Some(&class_only), 10).unwrap();
    assert!(!hits.is_empty());
    assert_eq!(hits.first().unwrap().display_name, "fixture::Square");
}

#[test]
fn definition_square_prefers_scope_location_over_token() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let engine = QueryEngine::new(&db).unwrap();
    let square = resolve_one(&db, "fixture::Square");

    let def = engine.definition(&[square]).unwrap();
    assert!(!def.locations.is_empty());
    for loc in &def.locations {
        assert_eq!(loc.location_kind, location_kind::SCOPE);
    }
}

#[test]
fn references_shape_locations_non_empty_and_point_at_real_fixture_source_paths() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let engine = QueryEngine::new(&db).unwrap();
    let shape = resolve_one(&db, "fixture::Shape");

    let refs = engine.references(&[shape]).unwrap();
    assert!(!refs.hits.is_empty());
    for hit in &refs.hits {
        assert!(!hit.location.file_path.is_empty());
        assert!(hit.location.file_path.contains("shapes.hpp"));
    }
}

#[test]
fn db_conformance_storage_version_all_13_tables_no_orphan_edges() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    assert_eq!(db.storage_version(), 25);

    let expected_tables = [
        "meta",
        "element",
        "element_component",
        "edge",
        "node",
        "symbol",
        "file",
        "filecontent",
        "local_symbol",
        "source_location",
        "occurrence",
        "component_access",
        "error",
    ];

    let mut present: BTreeSet<String> = BTreeSet::new();
    db.run("SELECT name FROM sqlite_master WHERE type = 'table'", [], |row| {
        present.insert(row.get::<_, Option<String>>(0)?.unwrap_or_default());
        Ok(())
    })
    .unwrap();

    for table in expected_tables {
        assert!(present.contains(table), "expected table '{}'", table);
    }

    let mut orphan_count = -1i64;
    db.run(
        "SELECT COUNT(*) FROM edge e LEFT JOIN node ns ON e.source_node_id = ns.id LEFT JOIN node nt ON e.target_node_id = nt.id WHERE ns.id IS NULL OR nt.id IS NULL",
        [],
        |row| {
            orphan_count = row.get::<_, i64>(0)?;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(orphan_count, 0);
}