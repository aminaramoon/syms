//! Integration tests for the petgraph-backed graph layer (src/graph/),
//! following tests/query_fixture.rs's conventions: the fixture database is
//! located via $SYMHOW_FIXTURE_DB with a `../cpp/build/fixture.srctrldb` fallback,
//! and assertions are pinned to the fixture ground truth documented in
//! docs/GRAPH_SCHEMA_NOTES.md §6 (independently re-verified against the real
//! database with SQL + a reference Brandes/PageRank/dominator implementation
//! before being hard-coded here).

use std::collections::BTreeSet;

use petgraph::visit::EdgeRef;

use symhow::db::enums::{edge_kind, node_kind};
use symhow::db::Database;
use symhow::graph::{
    backward_slice, betweenness, call_graph_dot, dominator_tree, induced_subgraph_dot,
    load_call_graph, personalized_pagerank, resolve_entry_points, shortest_path, BackwardSlice,
    CallGraph, DominatorTree, GraphLoadOptions, PageRankParams, PathWeight, ScoredNode,
    WeightedPath,
};

fn fixture_db_path() -> String {
    std::env::var("SYMHOW_FIXTURE_DB").unwrap_or_else(|_| "../cpp/build/fixture.srctrldb".to_string())
}

fn open_fixture() -> Database {
    Database::open(fixture_db_path(), 0).unwrap()
}

// Fixture node ids (docs/GRAPH_SCHEMA_NOTES.md §6; sanity-checked by
// fixture_ids_still_point_at_the_expected_symbols below).
const UNSOLVED_HUB: i64 = 141;
const SQUARE_AREA: i64 = 27;
const SQUARE_PERIMETER: i64 = 33;
const G_GLOBAL_COUNTER: i64 = 59;
const USE_EVERYTHING: i64 = 60;
const LEVEL4: i64 = 147;
const LEVEL3: i64 = 148;
const LEVEL2: i64 = 149;
const LEVEL1: i64 = 150;
const LEVEL0: i64 = 151;
const FACTORIAL: i64 = 152;
const PONG: i64 = 153;
const PING: i64 = 154;
const RUN_CHAIN: i64 = 155;
const BOX_TOUCH: i64 = 180;
const HELPER_TU_A: i64 = 205;
const MAIN_TU_A: i64 = 206;
const HELPER_TU_B: i64 = 210;
const MAIN_TU_B: i64 = 211;
const ACCESSOR_VALUE: i64 = 108;
const ACCESSOR_VALUE_CONST: i64 = 112;

fn default_options() -> GraphLoadOptions {
    GraphLoadOptions::default() // include_usage, no body lines, unsolved excluded
}

fn load_default() -> CallGraph {
    let db = open_fixture();
    load_call_graph(&db, &default_options()).unwrap()
}

fn score_of(ranked: &[ScoredNode], node_id: i64) -> f64 {
    ranked
        .iter()
        .find(|n| n.node_id == node_id)
        .unwrap_or_else(|| panic!("node {} missing from ranked list", node_id))
        .score
}

fn edge_triples(graph: &CallGraph) -> BTreeSet<(i64, i32, i64)> {
    graph
        .graph
        .edge_references()
        .map(|e| {
            (
                graph.graph[e.source()],
                e.weight().kind,
                graph.graph[e.target()],
            )
        })
        .collect()
}

// --- ids sanity -----------------------------------------------------------------

#[test]
fn fixture_ids_still_point_at_the_expected_symbols() {
    let graph = load_default();
    let expect = [
        (RUN_CHAIN, "runChain"),
        (LEVEL0, "level0"),
        (LEVEL1, "level1"),
        (LEVEL2, "level2"),
        (LEVEL3, "level3"),
        (LEVEL4, "level4"),
        (FACTORIAL, "factorial"),
        (PING, "ping"),
        (PONG, "pong"),
        (SQUARE_AREA, "area"),
        (SQUARE_PERIMETER, "squarePerimeter"),
        (USE_EVERYTHING, "useEverything"),
        (G_GLOBAL_COUNTER, "gGlobalCounter"),
        (BOX_TOUCH, "touch"),
        (MAIN_TU_A, "main"),
        (MAIN_TU_B, "main"),
        (HELPER_TU_A, "helper"),
        (HELPER_TU_B, "helper"),
        (ACCESSOR_VALUE, "value"),
        (ACCESSOR_VALUE_CONST, "value"),
    ];
    for (id, name) in expect {
        let node = graph
            .node_data(id)
            .unwrap_or_else(|| panic!("node {} ({}) not a participant", id, name));
        assert_eq!(node.name, name, "node {} has the wrong leaf name", id);
    }
}

// --- loading --------------------------------------------------------------------

#[test]
fn default_load_has_48_nodes_39_edges_and_excludes_the_unsolved_hub() {
    let graph = load_default();
    assert_eq!(graph.node_count(), 48);
    assert_eq!(graph.edge_count(), 39);

    // 28 CALL + 11 USAGE edges (31 CALL minus the 3 pointing at the hub).
    let triples = edge_triples(&graph);
    assert_eq!(
        triples
            .iter()
            .filter(|(_, k, _)| *k == edge_kind::CALL)
            .count(),
        28
    );
    assert_eq!(
        triples
            .iter()
            .filter(|(_, k, _)| *k == edge_kind::USAGE)
            .count(),
        11
    );

    // The unsolved hub is excluded entirely.
    assert_eq!(graph.excluded_unsolved_ids, vec![UNSOLVED_HUB]);
    assert!(graph.index(UNSOLVED_HUB).is_none());
    assert!(graph.node_data(UNSOLVED_HUB).is_none());
    assert!(!triples
        .iter()
        .any(|(s, _, t)| *s == UNSOLVED_HUB || *t == UNSOLVED_HUB));
}

#[test]
fn include_unsolved_load_adds_the_hub_and_its_three_call_edges() {
    let db = open_fixture();
    let options = GraphLoadOptions {
        exclude_unsolved: false,
        ..default_options()
    };
    let graph = load_call_graph(&db, &options).unwrap();
    assert_eq!(graph.node_count(), 49); // +1 hub node
    assert_eq!(graph.edge_count(), 42); // +3 CALL edges into the hub
    assert!(graph.excluded_unsolved_ids.is_empty());
    assert!(graph.index(UNSOLVED_HUB).is_some());

    let hub_edges: Vec<(i64, i32, i64)> = graph
        .graph
        .edge_references()
        .map(|e| {
            (
                graph.graph[e.source()],
                e.weight().kind,
                graph.graph[e.target()],
            )
        })
        .filter(|(s, _, t)| *s == UNSOLVED_HUB || *t == UNSOLVED_HUB)
        .collect();
    assert_eq!(hub_edges.len(), 3);
    assert!(hub_edges
        .iter()
        .all(|(_, k, t)| *k == edge_kind::CALL && *t == UNSOLVED_HUB));
}

#[test]
fn call_only_load_has_40_callable_nodes_28_call_edges() {
    let db = open_fixture();
    let options = GraphLoadOptions {
        include_usage: false,
        ..default_options()
    };
    let graph = load_call_graph(&db, &options).unwrap();
    assert_eq!(graph.node_count(), 40); // every FUNCTION/METHOD node
    assert_eq!(graph.edge_count(), 28);

    // Without USAGE edges the participants are exactly the callables, and
    // every loaded edge is a CALL edge.
    for id in graph.index_of.keys() {
        let node = graph.node_data(*id).unwrap();
        assert_ne!(
            node.kind & (node_kind::FUNCTION | node_kind::METHOD),
            0,
            "participant {} is not callable",
            id
        );
    }
    for reference in graph.graph.edge_references() {
        assert_eq!(reference.weight().kind, edge_kind::CALL);
    }
    // USAGE-only participants (the global counter) are gone.
    assert!(graph.index(G_GLOBAL_COUNTER).is_none());
}

#[test]
fn repeated_loads_are_deterministic() {
    let a = load_default();
    let b = load_default();
    assert_eq!(
        a.index_of.keys().collect::<BTreeSet<_>>(),
        b.index_of.keys().collect::<BTreeSet<_>>()
    );
    let a_edges: BTreeSet<(i64, i64, i32, i64)> = a
        .graph
        .edge_references()
        .map(|e| {
            (
                a.graph[e.source()],
                a.graph[e.target()],
                e.weight().kind,
                e.weight().edge_id,
            )
        })
        .collect();
    let b_edges: BTreeSet<(i64, i64, i32, i64)> = b
        .graph
        .edge_references()
        .map(|e| {
            (
                b.graph[e.source()],
                b.graph[e.target()],
                e.weight().kind,
                e.weight().edge_id,
            )
        })
        .collect();
    assert_eq!(a_edges, b_edges);
}

#[test]
fn with_body_lines_populates_scope_spans() {
    let db = open_fixture();
    let with = load_call_graph(
        &db,
        &GraphLoadOptions {
            with_body_lines: true,
            ..default_options()
        },
    )
    .unwrap();
    // The chain functions are 4 lines each; factorial/ping/pong 8;
    // useEverything 10; Square::area 1 (single-line body).
    for id in [LEVEL0, LEVEL1, LEVEL2, LEVEL3, LEVEL4, RUN_CHAIN] {
        assert_eq!(
            with.node_data(id).unwrap().body_lines,
            Some(4),
            "node {}",
            id
        );
    }
    assert_eq!(with.node_data(FACTORIAL).unwrap().body_lines, Some(8));
    assert_eq!(with.node_data(PING).unwrap().body_lines, Some(8));
    assert_eq!(with.node_data(PONG).unwrap().body_lines, Some(8));
    assert_eq!(with.node_data(USE_EVERYTHING).unwrap().body_lines, Some(10));
    assert_eq!(with.node_data(SQUARE_AREA).unwrap().body_lines, Some(1));

    // Default: not loaded.
    let without = load_default();
    assert_eq!(without.node_data(LEVEL0).unwrap().body_lines, None);
}

// --- betweenness centrality -------------------------------------------------------

/// Hand-computed Brandes scores over the default (CALL+USAGE) fixture graph,
/// independently re-derived with a reference implementation and SQL.
#[test]
fn betweenness_matches_the_hand_computed_brandes_scores() {
    let graph = load_default();
    let ranked = betweenness(&graph).unwrap();

    assert_eq!(ranked.len(), 48); // every participant is reported, even at 0.0

    let expected = [
        (LEVEL2, 6.0),
        (LEVEL1, 6.0),
        (LEVEL3, 4.0),
        (LEVEL0, 4.0),
        (SQUARE_AREA, 2.0),
        (PING, 1.0),
        (BOX_TOUCH, 1.0),
        (ACCESSOR_VALUE, 0.5),
        (ACCESSOR_VALUE_CONST, 0.5),
    ];
    for (id, score) in expected {
        assert!(
            (score_of(&ranked, id) - score).abs() < 1e-9,
            "betweenness of node {} is {}, expected {}",
            id,
            score_of(&ranked, id),
            score
        );
    }

    // Everyone else scores 0 (mains, helpers, leaves, the chain ends, ...).
    for node in ranked.iter().filter(|n| n.score == 0.0) {
        assert!(!expected.iter().any(|(id, _)| *id == node.node_id));
    }
    assert_eq!(
        ranked.iter().filter(|n| n.score > 0.0).count(),
        expected.len()
    );

    // Ranking: score descending, ties broken by ascending node id.
    let top: Vec<i64> = ranked.iter().take(9).map(|n| n.node_id).collect();
    assert_eq!(
        top,
        vec![
            LEVEL2,
            LEVEL1,
            LEVEL3,
            LEVEL0,
            SQUARE_AREA,
            PING,
            BOX_TOUCH,
            ACCESSOR_VALUE,
            ACCESSOR_VALUE_CONST
        ]
    );
    for pair in ranked.windows(2) {
        assert!(
            pair[0].score > pair[1].score
                || (pair[0].score == pair[1].score && pair[0].node_id < pair[1].node_id),
            "ranking violated between {:?} and {:?}",
            pair[0],
            pair[1]
        );
    }
}

// --- personalized PageRank --------------------------------------------------------

#[test]
fn personalized_pagerank_on_both_mains_conserves_mass_and_ranks_mains_top() {
    let graph = load_default();
    let ranked =
        personalized_pagerank(&graph, &[MAIN_TU_A, MAIN_TU_B], &Default::default()).unwrap();

    // Mass conservation.
    let total: f64 = ranked.iter().map(|n| n.score).sum();
    assert!((total - 1.0).abs() < 1e-6, "rank mass sums to {}", total);

    // The two TU-local mains tie at the top (~0.2703 each), then the helpers.
    assert_eq!(ranked[0].node_id, MAIN_TU_A);
    assert_eq!(ranked[1].node_id, MAIN_TU_B);
    assert!(
        (ranked[0].score - ranked[1].score).abs() < 1e-9,
        "mains must tie"
    );
    assert!(
        (ranked[0].score - 0.270270).abs() < 1e-4,
        "main score {}",
        ranked[0].score
    );
    assert_eq!(ranked[2].node_id, HELPER_TU_A);
    assert_eq!(ranked[3].node_id, HELPER_TU_B);
    assert!(
        (ranked[2].score - 0.229730).abs() < 1e-4,
        "helper score {}",
        ranked[2].score
    );

    // Everything outside the mains' component gets no mass.
    for id in [
        RUN_CHAIN,
        LEVEL0,
        SQUARE_AREA,
        USE_EVERYTHING,
        G_GLOBAL_COUNTER,
    ] {
        assert_eq!(score_of(&ranked, id), 0.0, "node {} must be unranked", id);
    }
}

#[test]
fn personalized_pagerank_damping_bounds_are_valid() {
    let graph = load_default();

    // damping = 0: pure teleport, the rank is exactly the personalization.
    let ranked = personalized_pagerank(
        &graph,
        &[MAIN_TU_A, MAIN_TU_B],
        &PageRankParams {
            damping: 0.0,
            ..Default::default()
        },
    )
    .unwrap();
    let total: f64 = ranked.iter().map(|n| n.score).sum();
    assert!((total - 1.0).abs() < 1e-9);
    assert!((score_of(&ranked, MAIN_TU_A) - 0.5).abs() < 1e-9);
    assert!((score_of(&ranked, MAIN_TU_B) - 0.5).abs() < 1e-9);
    assert_eq!(score_of(&ranked, HELPER_TU_A), 0.0);

    // damping = 1: no teleport at all. With dangling nodes (helper_a has no
    // outgoing edges) the iteration provably oscillates between {main_a} and
    // {helper_a} instead of converging (the dangling mass keeps bouncing back
    // through the personalization), so only the invariants are asserted:
    // mass stays conserved and confined to the seed's component.
    let ranked = personalized_pagerank(
        &graph,
        &[MAIN_TU_A],
        &PageRankParams {
            damping: 1.0,
            ..Default::default()
        },
    )
    .unwrap();
    let total: f64 = ranked.iter().map(|n| n.score).sum();
    assert!((total - 1.0).abs() < 1e-6, "rank mass sums to {}", total);
    let pair = score_of(&ranked, MAIN_TU_A) + score_of(&ranked, HELPER_TU_A);
    assert!(
        (pair - 1.0).abs() < 1e-6,
        "mass left the seed component: {}",
        pair
    );
    assert_eq!(score_of(&ranked, RUN_CHAIN), 0.0);
}

#[test]
fn personalized_pagerank_rejects_damping_outside_the_unit_range() {
    let graph = load_default();
    for damping in [-0.1f64, 1.0001, 42.0] {
        let err = personalized_pagerank(
            &graph,
            &[MAIN_TU_A],
            &PageRankParams {
                damping,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            err.0.contains("outside [0, 1]"),
            "damping {} should be rejected, got: {}",
            damping,
            err.0
        );
    }
}

#[test]
fn personalized_pagerank_without_seeds_errors() {
    let graph = load_default();
    let err = personalized_pagerank(&graph, &[], &Default::default()).unwrap_err();
    assert!(err.0.contains("no seed entry points"), "error: {}", err.0);
}

#[test]
fn personalized_pagerank_with_only_non_participant_seeds_errors() {
    let graph = load_default();
    // The unsolved hub is excluded from the default graph.
    let err = personalized_pagerank(&graph, &[UNSOLVED_HUB], &Default::default()).unwrap_err();
    assert!(err.0.contains("no seed entry points"), "error: {}", err.0);
}

// --- dominator tree ----------------------------------------------------------------

#[test]
fn dominator_tree_from_runchain_has_exactly_the_eight_expected_idoms() {
    let graph = load_default();
    let tree = dominator_tree(&graph, RUN_CHAIN).unwrap();
    assert_eq!(tree.root, RUN_CHAIN);

    let expected: BTreeSet<(i64, i64)> = BTreeSet::from([
        (LEVEL0, RUN_CHAIN),
        (FACTORIAL, RUN_CHAIN),
        (PING, RUN_CHAIN),
        (LEVEL1, LEVEL0),
        (LEVEL2, LEVEL1),
        (LEVEL3, LEVEL2),
        (LEVEL4, LEVEL3),
        (PONG, PING),
    ]);
    let got: BTreeSet<(i64, i64)> = tree.idoms.iter().copied().collect();
    assert_eq!(got, expected);

    // The root itself has no idom entry, and unreachable nodes are absent.
    assert!(!got.iter().any(|(node, _)| *node == RUN_CHAIN));
    assert!(!got.iter().any(|(node, _)| *node == MAIN_TU_A));
}

#[test]
fn dominator_tree_rejects_non_participant_root() {
    let graph = load_default();
    let err = dominator_tree(&graph, UNSOLVED_HUB).unwrap_err();
    assert!(err.0.contains("not a participant"), "error: {}", err.0);
}

// --- backward slicing ---------------------------------------------------------------

#[test]
fn backward_slice_of_global_counter_is_the_pair_59_60() {
    let graph = load_default();
    let slice = backward_slice(&graph, G_GLOBAL_COUNTER).unwrap();
    assert_eq!(slice.criterion, G_GLOBAL_COUNTER);
    assert_eq!(slice.nodes, vec![G_GLOBAL_COUNTER, USE_EVERYTHING]);

    // One dependence edge: useEverything --USAGE--> gGlobalCounter, with its
    // reference site (ns.cpp:65) attached.
    assert_eq!(slice.edges.len(), 1);
    let edge = &slice.edges[0];
    assert_eq!(edge.source, USE_EVERYTHING);
    assert_eq!(edge.target, G_GLOBAL_COUNTER);
    assert_eq!(edge.kind, edge_kind::USAGE);
    assert_eq!(edge.call_sites, 1);
    assert_eq!(edge.sites.len(), 1);
    assert!(
        edge.sites[0].file_path.ends_with("ns.cpp"),
        "site: {:?}",
        edge.sites[0]
    );
    assert_eq!(edge.sites[0].start_line, 65);
    assert_eq!(edge.sites[0].end_line, 65);
}

#[test]
fn backward_slice_of_square_area_is_the_triple_with_parallel_call_usage_edges() {
    let graph = load_default();
    let slice = backward_slice(&graph, SQUARE_AREA).unwrap();
    assert_eq!(
        slice.nodes,
        vec![SQUARE_AREA, SQUARE_PERIMETER, USE_EVERYTHING]
    );

    let edges: BTreeSet<(i64, i32, i64)> = slice
        .edges
        .iter()
        .map(|e| (e.source, e.kind, e.target))
        .collect();
    let expected: BTreeSet<(i64, i32, i64)> = BTreeSet::from([
        (SQUARE_PERIMETER, edge_kind::CALL, SQUARE_AREA),
        (USE_EVERYTHING, edge_kind::CALL, SQUARE_AREA),
        // The parallel CALL+USAGE pair useEverything -> squarePerimeter:
        // both are present and both carry their site lines.
        (USE_EVERYTHING, edge_kind::CALL, SQUARE_PERIMETER),
        (USE_EVERYTHING, edge_kind::USAGE, SQUARE_PERIMETER),
    ]);
    assert_eq!(edges, expected);
    assert_eq!(slice.edges.len(), 4);

    let by_pair: std::collections::HashMap<(i64, i64, i32), &symhow::graph::SliceEdge> = slice
        .edges
        .iter()
        .map(|e| ((e.source, e.target, e.kind), e))
        .collect();
    let call_33_27 = by_pair[&(SQUARE_PERIMETER, SQUARE_AREA, edge_kind::CALL)];
    assert_eq!(call_33_27.sites.len(), 1);
    assert_eq!(call_33_27.sites[0].start_line, 55);
    for edge in [
        by_pair[&(USE_EVERYTHING, SQUARE_AREA, edge_kind::CALL)],
        by_pair[&(USE_EVERYTHING, SQUARE_PERIMETER, edge_kind::CALL)],
        by_pair[&(USE_EVERYTHING, SQUARE_PERIMETER, edge_kind::USAGE)],
    ] {
        assert_eq!(edge.sites.len(), 1);
        assert_eq!(edge.sites[0].start_line, 66);
        assert!(edge.sites[0].file_path.ends_with("ns.cpp"));
    }
}

#[test]
fn backward_slice_rejects_non_participant_criterion() {
    let graph = load_default();
    let err = backward_slice(&graph, UNSOLVED_HUB).unwrap_err();
    assert!(err.0.contains("not a participant"), "error: {}", err.0);
}

// --- weighted shortest path -----------------------------------------------------------

#[test]
fn shortest_path_runchain_to_level4_under_all_three_weight_schemes() {
    let db = open_fixture();
    let graph = load_call_graph(
        &db,
        &GraphLoadOptions {
            with_body_lines: true,
            ..default_options()
        },
    )
    .unwrap();

    let expected_nodes = [RUN_CHAIN, LEVEL0, LEVEL1, LEVEL2, LEVEL3, LEVEL4];
    let expected_costs = [
        (PathWeight::Hops, 5.0),
        (PathWeight::Calls, 5.0),
        (PathWeight::Lines, 20.0),
    ];
    for (scheme, cost) in expected_costs {
        let path = shortest_path(&graph, RUN_CHAIN, LEVEL4, scheme)
            .unwrap()
            .unwrap_or_else(|| panic!("{:?} path should exist", scheme));
        assert_eq!(path.source, RUN_CHAIN);
        assert_eq!(path.target, LEVEL4);
        assert_eq!(path.scheme, scheme);
        assert!(
            (path.total_cost - cost).abs() < 1e-9,
            "{:?}: {}",
            scheme,
            path.total_cost
        );
        assert_eq!(path.nodes, expected_nodes);
        assert_eq!(path.edge_ids.len(), 5);
        assert!(path.edge_ids.iter().all(|&id| id != 0));
    }
}

#[test]
fn shortest_path_to_a_disconnected_node_is_none() {
    let graph = load_default();
    // runChain's component does not reach the TU-local mains/helpers.
    assert_eq!(
        shortest_path(&graph, RUN_CHAIN, HELPER_TU_A, PathWeight::Hops).unwrap(),
        None
    );
    assert_eq!(
        shortest_path(&graph, MAIN_TU_A, RUN_CHAIN, PathWeight::Hops).unwrap(),
        None
    );
}

#[test]
fn shortest_path_lines_requires_body_lines_to_be_loaded() {
    let db = open_fixture();
    let graph = load_call_graph(&db, &default_options()).unwrap(); // with_body_lines: false
    let err = shortest_path(&graph, RUN_CHAIN, LEVEL4, PathWeight::Lines).unwrap_err();
    assert!(err.0.contains("with_body_lines"), "error: {}", err.0);
}

#[test]
fn shortest_path_rejects_non_participant_endpoints() {
    let graph = load_default();
    let err = shortest_path(&graph, RUN_CHAIN, UNSOLVED_HUB, PathWeight::Hops).unwrap_err();
    assert!(err.0.contains("not a participant"), "error: {}", err.0);
    let err = shortest_path(&graph, UNSOLVED_HUB, LEVEL4, PathWeight::Hops).unwrap_err();
    assert!(err.0.contains("not a participant"), "error: {}", err.0);
}

// --- seed resolution -------------------------------------------------------------------

#[test]
fn resolve_entry_points_explicit_seeds_accept_ambiguous_names_and_id_specs() {
    let db = open_fixture();
    let graph = load_default();

    // "main" is ambiguous (two TU-local mains) — every participating
    // candidate counts as an entry point.
    let seeds = resolve_entry_points(&db, &graph, &["main".to_string()]).unwrap();
    assert_eq!(seeds, vec![MAIN_TU_A, MAIN_TU_B]);

    // Direct '#<id>' specs resolve to exactly one node each.
    let seeds = resolve_entry_points(&db, &graph, &["#206".to_string()]).unwrap();
    assert_eq!(seeds, vec![MAIN_TU_A]);
    let seeds =
        resolve_entry_points(&db, &graph, &["#206".to_string(), "#211".to_string()]).unwrap();
    assert_eq!(seeds, vec![MAIN_TU_A, MAIN_TU_B]);

    // Duplicate specs dedup.
    let seeds =
        resolve_entry_points(&db, &graph, &["#206".to_string(), "#206".to_string()]).unwrap();
    assert_eq!(seeds, vec![MAIN_TU_A]);
}

#[test]
fn resolve_entry_points_without_seeds_uses_the_main_heuristic() {
    let db = open_fixture();
    let graph = load_default();
    let seeds = resolve_entry_points(&db, &graph, &[]).unwrap();
    assert_eq!(seeds, vec![MAIN_TU_A, MAIN_TU_B]);
}

#[test]
fn resolve_entry_points_unknown_specs_fall_through_to_the_main_heuristic() {
    let db = open_fixture();
    let graph = load_default();
    // Nothing resolves -> the fallback (FUNCTION nodes named `main`) runs.
    let seeds = resolve_entry_points(&db, &graph, &["no_such_symbol_xyz".to_string()]).unwrap();
    assert_eq!(seeds, vec![MAIN_TU_A, MAIN_TU_B]);
}

// --- edge cases: empty DB, disconnected components, cycles, DOT -----------------------

/// Builds a minimal Sourcetrail-schema database with exactly the tables the
/// graph loader reads, containing the given (type, serialized_name) nodes and
/// (type, source, target) edges.
fn build_synthetic_db(
    path: &std::path::Path,
    nodes: &[(i64, i32, &str)],
    edges: &[(i64, i32, i64, i64)],
) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE meta (id INTEGER PRIMARY KEY, key TEXT, value TEXT);
         INSERT INTO meta (key, value) VALUES ('storage_version', '25');
         CREATE TABLE node (id INTEGER PRIMARY KEY, type INTEGER, serialized_name TEXT);
         CREATE TABLE edge (id INTEGER PRIMARY KEY, type INTEGER, source_node_id INTEGER, target_node_id INTEGER);
         CREATE TABLE occurrence (element_id INTEGER, source_location_id INTEGER);
         CREATE TABLE source_location (
             id INTEGER PRIMARY KEY, file_node_id INTEGER, start_line INTEGER,
             start_column INTEGER, end_line INTEGER, end_column INTEGER, type INTEGER);
         CREATE TABLE file (id INTEGER PRIMARY KEY, path TEXT);",
    )
    .unwrap();
    for (id, kind, name) in nodes {
        conn.execute(
            "INSERT INTO node (id, type, serialized_name) VALUES (?, ?, ?)",
            rusqlite::params![id, kind, format!("::\tm{}\ts\tp", name)],
        )
        .unwrap();
    }
    for (id, kind, source, target) in edges {
        conn.execute(
            "INSERT INTO edge (id, type, source_node_id, target_node_id) VALUES (?, ?, ?, ?)",
            rusqlite::params![id, kind, source, target],
        )
        .unwrap();
    }
}

fn synthetic_db_path(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("symhow_graph_analysis_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn function(id: i64, name: &'static str) -> (i64, i32, &'static str) {
    (id, node_kind::FUNCTION, name)
}

#[test]
fn empty_database_loads_an_empty_graph_and_algorithms_error_cleanly() {
    let path = synthetic_db_path("empty.srctrldb");
    let _ = std::fs::remove_file(&path);
    build_synthetic_db(&path, &[], &[]);
    let db = Database::open(path.to_string_lossy().as_ref(), 0).unwrap();

    let graph = load_call_graph(&db, &default_options()).unwrap();
    assert_eq!(graph.node_count(), 0);
    assert_eq!(graph.edge_count(), 0);

    assert!(betweenness(&graph).unwrap().is_empty());
    assert!(personalized_pagerank(&graph, &[], &Default::default()).is_err());
    assert!(dominator_tree(&graph, 1).is_err());
    assert!(backward_slice(&graph, 1).is_err());
    assert!(shortest_path(&graph, 1, 2, PathWeight::Hops).is_err());
    assert_eq!(
        resolve_entry_points(&db, &graph, &[]).unwrap(),
        Vec::<i64>::new()
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn synthetic_diamond_with_isolated_node_and_cycle_is_handled() {
    // 1 -> {2, 3} -> 4 (diamond), 5 isolated, 6 <=> 7 (2-cycle).
    let path = synthetic_db_path("diamond.srctrldb");
    let _ = std::fs::remove_file(&path);
    build_synthetic_db(
        &path,
        &[
            function(1, "alpha"),
            function(2, "bravo"),
            function(3, "charlie"),
            function(4, "delta"),
            function(5, "isolated"),
            function(6, "echo"),
            function(7, "foxtrot"),
        ],
        &[
            (100, edge_kind::CALL, 1, 2),
            (101, edge_kind::CALL, 1, 3),
            (102, edge_kind::CALL, 2, 4),
            (103, edge_kind::CALL, 3, 4),
            (104, edge_kind::CALL, 6, 7),
            (105, edge_kind::CALL, 7, 6),
        ],
    );
    let db = Database::open(path.to_string_lossy().as_ref(), 0).unwrap();
    let graph = load_call_graph(&db, &default_options()).unwrap();
    assert_eq!(graph.node_count(), 7);
    assert_eq!(graph.edge_count(), 6);

    // Betweenness: the diamond's mid nodes split the 1 -> 4 traffic.
    let ranked = betweenness(&graph).unwrap();
    assert!((score_of(&ranked, 2) - 0.5).abs() < 1e-9);
    assert!((score_of(&ranked, 3) - 0.5).abs() < 1e-9);
    for id in [1, 4, 5, 6, 7] {
        assert_eq!(score_of(&ranked, id), 0.0, "node {}", id);
    }

    // Dominators from the diamond's source: the isolated node and the cycle
    // are unreachable and absent; the cycle itself is handled fine.
    let tree = dominator_tree(&graph, 1).unwrap();
    let got: BTreeSet<(i64, i64)> = tree.idoms.iter().copied().collect();
    assert_eq!(got, BTreeSet::from([(2, 1), (3, 1), (4, 1)]));
    let cycle_tree = dominator_tree(&graph, 6).unwrap();
    assert_eq!(cycle_tree.idoms, vec![(7, 6)]);

    // PageRank seeded on the diamond source: mass stays in the reachable set,
    // the isolated node gets nothing, and the cycle does not diverge.
    let ranked = personalized_pagerank(&graph, &[1], &Default::default()).unwrap();
    let total: f64 = ranked.iter().map(|n| n.score).sum();
    assert!((total - 1.0).abs() < 1e-6);
    assert_eq!(score_of(&ranked, 5), 0.0);

    // Shortest path through the diamond, and no path to the isolated node.
    let hop_path = shortest_path(&graph, 1, 4, PathWeight::Hops)
        .unwrap()
        .unwrap();
    assert_eq!(hop_path.total_cost, 2.0);
    assert_eq!(shortest_path(&graph, 1, 5, PathWeight::Hops).unwrap(), None);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn missing_and_empty_database_files_fail_with_clean_errors() {
    let missing = synthetic_db_path("does_not_exist.srctrldb");
    let _ = std::fs::remove_file(&missing);
    let err = Database::open(missing.to_string_lossy().as_ref(), 0)
        .err()
        .unwrap_or_else(|| panic!("opening a missing database must fail"));
    assert!(err.0.contains("failed to open"), "error: {}", err.0);

    let empty = synthetic_db_path("empty_file.srctrldb");
    std::fs::write(&empty, b"").unwrap();
    let err = Database::open(empty.to_string_lossy().as_ref(), 0)
        .err()
        .unwrap_or_else(|| panic!("opening an empty file must fail"));
    assert!(
        err.0.contains("not a valid Sourcetrail-schema database")
            || err
                .0
                .contains("does not look like a Sourcetrail-schema database"),
        "error: {}",
        err.0
    );
    let _ = std::fs::remove_file(&empty);
}

#[test]
fn dot_rendering_contains_display_labels_and_rejects_foreign_ids() {
    let graph = load_default();

    let dot = call_graph_dot(&graph);
    assert!(dot.starts_with("digraph"));
    assert!(dot.contains("runChain() (FUNCTION)"));
    assert!(dot.contains("level2() (FUNCTION)"));
    assert!(dot.contains("CALL"));
    // The excluded hub must not appear.
    assert!(!dot.contains("unsolved"));

    // The slice {27, 33, 60} induced subgraph mentions exactly its members.
    let dot =
        induced_subgraph_dot(&graph, &[SQUARE_AREA, SQUARE_PERIMETER, USE_EVERYTHING]).unwrap();
    assert!(dot.contains("Square::area() const (METHOD)"));
    assert!(dot.contains("squarePerimeter(const Square &) (FUNCTION)"));
    assert!(dot.contains("useEverything() (FUNCTION)"));
    assert!(!dot.contains("runChain"));

    // A non-participant id is an error.
    assert!(induced_subgraph_dot(&graph, &[UNSOLVED_HUB]).is_err());
}

#[test]
fn fixture_cycles_are_present_in_the_loaded_graph() {
    let graph = load_default();
    let triples = edge_triples(&graph);
    // factorial self-loop and the ping <=> pong 2-cycle.
    assert!(triples.contains(&(FACTORIAL, edge_kind::CALL, FACTORIAL)));
    assert!(triples.contains(&(PING, edge_kind::CALL, PONG)));
    assert!(triples.contains(&(PONG, edge_kind::CALL, PING)));
    // The chain runChain -> level0 -> ... -> level4.
    for (from, to) in [
        (RUN_CHAIN, LEVEL0),
        (LEVEL0, LEVEL1),
        (LEVEL1, LEVEL2),
        (LEVEL2, LEVEL3),
        (LEVEL3, LEVEL4),
    ] {
        assert!(triples.contains(&(from, edge_kind::CALL, to)));
    }
    // Both TU-local mains call their own TU's helper (and only that one).
    assert!(triples.contains(&(MAIN_TU_A, edge_kind::CALL, HELPER_TU_A)));
    assert!(triples.contains(&(MAIN_TU_B, edge_kind::CALL, HELPER_TU_B)));
    assert!(!triples.contains(&(MAIN_TU_A, edge_kind::CALL, HELPER_TU_B)));
    assert!(!triples.contains(&(MAIN_TU_B, edge_kind::CALL, HELPER_TU_A)));
}

// Keep the unused-import lint honest about the result types used above.
#[allow(dead_code)]
fn type_witness(_: BackwardSlice, _: DominatorTree, _: WeightedPath) {}
