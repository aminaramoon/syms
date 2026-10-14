//! Port of test/unit/test_json_shape.cpp (fixture-dependent, [json_shape]).
//!
//! For each of the symhow.*/1 schemas: build the result via the real
//! QueryEngine call, serialize via JsonWriter, round-trip through
//! serde_json::from_str (proves it's valid JSON), and assert the schema
//! name, required top-level keys, and that every "kind" field anywhere in
//! the tree is a JSON string (never a number).

use std::collections::HashMap;

use serde_json::{json, Value};
use symhow::db::Database;
use symhow::db::enums::{edge_kind, node_kind};
use symhow::out::{JsonWriter, QueryContext};
use symhow::query::{
    resolve_file_path, resolve_symbol, QueryEngine, ResolveOutcome,
};

fn fixture_db_path() -> String {
    std::env::var("SYMHOW_FIXTURE_DB").unwrap_or_else(|_| "../build/fixture.srctrldb".to_string())
}

fn resolve_one(db: &Database, spec: &str) -> i64 {
    let rr = resolve_symbol(db, spec, None, None).unwrap();
    assert_eq!(rr.outcome, ResolveOutcome::Unique, "resolving '{}'", spec);
    rr.candidates[0].id
}

// Walks the entire JSON tree and asserts every value stored under a key
// literally named "kind" is a JSON string -- covers node.kind, edge.kind,
// location.kind, hit.node.kind, etc. wherever they occur, without having to
// enumerate each schema's exact nesting by hand.
fn check_kind_fields_are_strings(j: &Value) {
    match j {
        Value::Object(map) => {
            for (key, value) in map {
                if key == "kind" && !value.is_null() {
                    assert!(
                        value.is_string(),
                        "key 'kind' with non-string value: {}",
                        value
                    );
                }
                check_kind_fields_are_strings(value);
            }
        }
        Value::Array(arr) => {
            for element in arr {
                check_kind_fields_are_strings(element);
            }
        }
        _ => {}
    }
}

fn check_top_level_keys(j: &Value, required: &[&str]) {
    for key in required {
        assert!(
            j.get(key).is_some(),
            "missing required top-level key '{}' in {}",
            key,
            j
        );
    }
}

#[test]
fn json_shape_symhow_graph_1_is_valid_has_required_keys_and_every_kind_field_is_a_string() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let engine = QueryEngine::new(&db).unwrap();
    let writer = JsonWriter::new(&db, &engine, "test-version");

    let level4 = resolve_one(&db, "level4");
    let result = engine.callers(&[level4], /*depth=*/ 2, /*max_nodes=*/ 0, /*node_kind_mask=*/ 0).unwrap();
    assert!(!result.nodes.is_empty());

    let ctx = QueryContext {
        verb: "callers".to_string(),
        db: fixture_db_path(),
        symbol_or_text_or_path: "level4".to_string(),
        symbol_ids: vec![level4],
        depth: 2,
        max_nodes: 0,
        edge_kinds: vec![edge_kind::CALL],
        ..QueryContext::default()
    };

    let j = writer.graph(&ctx, &result, 7, None).unwrap();
    let dumped = j.to_string();
    let parsed: Value = serde_json::from_str(&dumped).unwrap(); // proves it's valid JSON

    assert_eq!(parsed["schema"], "symhow.graph/1");
    check_top_level_keys(
        &parsed,
        &["symhow_version", "schema", "query", "truncated", "stats", "nodes", "edges"],
    );
    assert_eq!(parsed["query"]["verb"], "callers");
    assert_eq!(parsed["stats"]["elapsed_ms"], 7);
    let nodes = parsed["nodes"].as_array().unwrap();
    assert!(!nodes.is_empty());
    for n in nodes {
        assert!(n.get("id").is_some());
        assert!(n["kind"].is_string());
        assert!(n.get("name").is_some());
        assert!(n.get("qualified_name").is_some());
        assert!(n.get("location").is_some()); // present, possibly null
    }
    check_kind_fields_are_strings(&parsed);
}

// Phase 4 of docs/NOTES_DESIGN.md: `symhow query context` reuses
// symhow.graph/1 verbatim, plus a "notes" field on every node when
// JsonWriter::graph() is given a notes map (the CLI builds this map from
// the notes store; this test builds a small map directly so it stays a pure
// JsonWriter unit test, independent of the notes markdown/DB cache
// plumbing exercised elsewhere).
#[test]
fn json_shape_symhow_graph_1_gains_a_notes_field_per_node_when_graph_is_given_a_notes_map() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let engine = QueryEngine::new(&db).unwrap();
    let writer = JsonWriter::new(&db, &engine, "test-version");

    let level2 = resolve_one(&db, "level2");
    let callers = engine.callers(&[level2], /*depth=*/ 1, /*max_nodes=*/ 0, 0).unwrap();
    let callees = engine.callees(&[level2], /*depth=*/ 1, /*max_nodes=*/ 0, 0).unwrap();
    let mut merged = callers;
    for n in &callees.nodes {
        let present = merged.nodes.iter().any(|m| m.id == n.id);
        if !present {
            merged.nodes.push(n.clone());
        }
    }
    assert!(merged.nodes.len() >= 2);

    // Fake a note for exactly the seed's serializedName.
    let seed_serialized_name = merged
        .nodes
        .iter()
        .find(|n| n.id == level2)
        .map(|n| n.serialized_name.clone())
        .unwrap_or_default();
    assert!(!seed_serialized_name.is_empty());

    let note = json!({
        "brief": "test brief",
        "what": "",
        "how": "",
        "signature": "",
        "pre": [],
        "post": [],
        "bugs": [],
        "suggestions": [],
        "path": "src/chain.cpp.md",
        "line": 1,
        "filled": true,
        "stale": false,
        "note_hash": "deadbeef",
        "current_body_hash": "deadbeef",
        "commit": "abc123",
    });
    let mut notes: HashMap<String, Value> = HashMap::new();
    notes.insert(seed_serialized_name, note);

    let ctx = QueryContext {
        verb: "context".to_string(),
        db: fixture_db_path(),
        symbol_or_text_or_path: "level2".to_string(),
        symbol_ids: vec![level2],
        depth: 1,
        max_nodes: 0,
        edge_kinds: vec![edge_kind::CALL],
        ..QueryContext::default()
    };

    let j1 = writer.graph(&ctx, &merged, 3, Some(&notes)).unwrap();
    let j2 = writer.graph(&ctx, &merged, 3, Some(&notes)).unwrap();
    assert_eq!(j1.to_string(), j2.to_string()); // deterministic across two runs

    let parsed: Value = serde_json::from_str(&j1.to_string()).unwrap();
    assert_eq!(parsed["schema"], "symhow.graph/1");
    assert_eq!(parsed["query"]["verb"], "context");
    let mut saw_filled_note = false;
    for n in parsed["nodes"].as_array().unwrap() {
        // present (object or null) on every node
        assert!(n.get("notes").is_some());
        if !n["notes"].is_null() {
            assert_eq!(n["notes"]["brief"], "test brief");
            saw_filled_note = true;
        }
    }
    assert!(saw_filled_note);
    check_kind_fields_are_strings(&parsed);

    // Without the notes map (as --no-notes uses), the field is absent
    // entirely rather than null.
    let j3 = writer.graph(&ctx, &merged, 3, None).unwrap();
    for n in j3["nodes"].as_array().unwrap() {
        assert!(n.get("notes").is_none());
    }
}

#[test]
fn json_shape_symhow_references_1_is_valid_and_has_the_declaration_synthetic_edge_kind() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let engine = QueryEngine::new(&db).unwrap();
    let writer = JsonWriter::new(&db, &engine, "test-version");

    let shape = resolve_one(&db, "fixture::Shape");
    let result = engine.references(&[shape]).unwrap();
    assert!(!result.hits.is_empty());

    let ctx = QueryContext {
        verb: "references".to_string(),
        db: fixture_db_path(),
        symbol_or_text_or_path: "fixture::Shape".to_string(),
        symbol_ids: vec![shape],
        ..QueryContext::default()
    };

    let j = writer.references(&ctx, &result, 4).unwrap();
    let parsed: Value = serde_json::from_str(&j.to_string()).unwrap();

    assert_eq!(parsed["schema"], "symhow.references/1");
    check_top_level_keys(
        &parsed,
        &["symhow_version", "schema", "query", "truncated", "stats", "nodes", "references"],
    );
    assert_eq!(parsed["truncated"], false);
    let mut saw_declaration = false;
    for r in parsed["references"].as_array().unwrap() {
        assert!(r.get("edge_kind").is_some());
        assert!(r["edge_kind"].is_string());
        if r["edge_kind"] == "DECLARATION" {
            saw_declaration = true;
            assert!(r["edge_id"].is_null());
        }
    }
    assert!(saw_declaration);
    check_kind_fields_are_strings(&parsed);
}

#[test]
fn json_shape_symhow_definition_1_has_a_single_symbol_object_and_sorted_locations() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let engine = QueryEngine::new(&db).unwrap();
    let writer = JsonWriter::new(&db, &engine, "test-version");

    let square = resolve_one(&db, "fixture::Square");
    let result = engine.definition(&[square]).unwrap();
    assert!(!result.locations.is_empty());

    let ctx = QueryContext {
        verb: "definition".to_string(),
        db: fixture_db_path(),
        symbol_or_text_or_path: "fixture::Square".to_string(),
        symbol_ids: vec![square],
        ..QueryContext::default()
    };

    let j = writer.definition(&ctx, &result, 0).unwrap();
    let parsed: Value = serde_json::from_str(&j.to_string()).unwrap();

    assert_eq!(parsed["schema"], "symhow.definition/1");
    check_top_level_keys(
        &parsed,
        &["symhow_version", "schema", "query", "truncated", "stats", "symbol", "locations"],
    );
    assert!(parsed["symbol"].is_object());
    assert_eq!(parsed["symbol"]["id"], square);
    assert_eq!(parsed["symbol"]["kind"], "CLASS");
    check_kind_fields_are_strings(&parsed);
}

#[test]
fn json_shape_symhow_search_1_hits_carry_matched_indices_and_a_full_node_object() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let engine = QueryEngine::new(&db).unwrap();
    let writer = JsonWriter::new(&db, &engine, "test-version");

    let class_only = vec![node_kind::CLASS];
    let hits = engine.search("sqr", Some(class_only.as_slice()), 10).unwrap();
    assert!(!hits.is_empty());

    let ctx = QueryContext {
        verb: "search".to_string(),
        db: fixture_db_path(),
        symbol_or_text_or_path: "sqr".to_string(),
        limit: 10,
        search_kinds: class_only.clone(),
        ..QueryContext::default()
    };

    let j = writer.search(&ctx, &hits, 61).unwrap();
    let parsed: Value = serde_json::from_str(&j.to_string()).unwrap();

    assert_eq!(parsed["schema"], "symhow.search/1");
    check_top_level_keys(
        &parsed,
        &["symhow_version", "schema", "query", "truncated", "stats", "hits"],
    );
    let hit_array = parsed["hits"].as_array().unwrap();
    assert!(!hit_array.is_empty());
    for h in hit_array {
        assert!(h.get("score").is_some());
        assert!(h["matched_indices"].is_array());
        assert!(h.get("node").is_some());
        assert!(h["node"]["kind"].is_string());
    }
    assert!(parsed["stats"]["candidates_scanned"].as_i64().unwrap() > 0);
    check_kind_fields_are_strings(&parsed);
}

#[test]
fn json_shape_symhow_stats_1_has_sparse_node_kinds_edge_kinds_and_a_database_object() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let engine = QueryEngine::new(&db).unwrap();
    let writer = JsonWriter::new(&db, &engine, "test-version");

    let result = engine.stats(100).unwrap();

    let ctx = QueryContext {
        verb: "stats".to_string(),
        db: fixture_db_path(),
        ..QueryContext::default()
    };

    let j = writer.stats(&ctx, &result).unwrap();
    let parsed: Value = serde_json::from_str(&j.to_string()).unwrap();

    assert_eq!(parsed["schema"], "symhow.stats/1");
    check_top_level_keys(
        &parsed,
        &["symhow_version", "schema", "query", "database", "counts", "node_kinds", "edge_kinds", "errors"],
    );
    assert_eq!(parsed["database"]["storage_version"], 25);
    assert_eq!(parsed["database"]["reader_indices_present"], true);
    assert!(parsed["counts"]["nodes"].as_i64().unwrap() > 0);
    // Sparse: every key present must have a nonzero count.
    if let Some(node_kinds) = parsed["node_kinds"].as_object() {
        for value in node_kinds.values() {
            assert!(value.as_i64().unwrap() > 0);
        }
    }
}

#[test]
fn json_shape_symhow_file_1_resolves_node_vs_is_local_symbol_occurrences_explicitly() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let engine = QueryEngine::new(&db).unwrap();
    let writer = JsonWriter::new(&db, &engine, "test-version");

    let rr = resolve_file_path(&db, "shapes.hpp").unwrap();
    assert_eq!(rr.outcome, ResolveOutcome::Unique);
    let file_id = rr.candidates[0].id;

    let fr = engine
        .file_info(file_id, /*want_contents=*/ true, /*want_occurrences=*/ true, None, None)
        .unwrap();
    assert!(!fr.occurrences.is_empty());

    let ctx = QueryContext {
        verb: "file".to_string(),
        db: fixture_db_path(),
        symbol_or_text_or_path: "shapes.hpp".to_string(),
        ..QueryContext::default()
    };

    let j = writer.file(&ctx, &fr, /*want_contents=*/ true, /*want_occurrences=*/ true).unwrap();
    let parsed: Value = serde_json::from_str(&j.to_string()).unwrap();

    assert_eq!(parsed["schema"], "symhow.file/1");
    check_top_level_keys(
        &parsed,
        &["symhow_version", "schema", "query", "file", "contents", "occurrences"],
    );
    let occurrences = parsed["occurrences"].as_array().unwrap();
    let mut saw_node = false;
    let mut saw_local_symbol = false;
    for occ in occurrences {
        assert!(occ.get("is_local_symbol").is_some());
        assert!(occ["is_local_symbol"].is_boolean());
        if let Some(node) = occ.get("node") {
            assert!(node["kind"].is_string());
            assert!(node.get("qualified_name").is_some());
            assert_eq!(occ["is_local_symbol"], false);
            saw_node = true;
        }
        if occ["is_local_symbol"] == true {
            assert!(occ.get("node").is_none());
            saw_local_symbol = true;
        }
    }
    assert!(saw_node);
    assert!(saw_local_symbol);
    check_kind_fields_are_strings(&parsed);
}

#[test]
fn json_shape_symhow_ambiguous_1_lists_every_candidate_as_a_full_node_object() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let engine = QueryEngine::new(&db).unwrap();
    let writer = JsonWriter::new(&db, &engine, "test-version");

    // "Square" (no --kind) is a known-ambiguous spec in the fixture -- the
    // class and its same-named constructor both have leaf name "Square"
    // (see docs/KNOWN_LIMITATIONS.md's Phase 4 section).
    let rr = resolve_symbol(&db, "Square", None, None).unwrap();
    assert_eq!(rr.outcome, ResolveOutcome::Ambiguous);
    assert!(rr.candidates.len() >= 2);

    let ctx = QueryContext {
        verb: "bases".to_string(),
        db: fixture_db_path(),
        symbol_or_text_or_path: "Square".to_string(),
        ..QueryContext::default()
    };

    let j = writer.ambiguous(&ctx, &rr.candidates).unwrap();
    let parsed: Value = serde_json::from_str(&j.to_string()).unwrap();

    assert_eq!(parsed["schema"], "symhow.ambiguous/1");
    check_top_level_keys(&parsed, &["symhow_version", "schema", "query", "candidates"]);
    let candidates = parsed["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), rr.candidates.len());
    for c in candidates {
        assert!(c["kind"].is_string());
        assert!(c.get("id").is_some());
    }
    check_kind_fields_are_strings(&parsed);
}

#[test]
fn json_shape_symhow_not_found_1_has_a_suggestions_array_possibly_empty() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let engine = QueryEngine::new(&db).unwrap();
    let writer = JsonWriter::new(&db, &engine, "test-version");

    let hits = engine.search("Squorable_Nonexistent_Xyz", None, 5).unwrap();

    let ctx = QueryContext {
        verb: "bases".to_string(),
        db: fixture_db_path(),
        symbol_or_text_or_path: "Squorable_Nonexistent_Xyz".to_string(),
        ..QueryContext::default()
    };

    let j = writer.not_found(&ctx, &hits).unwrap();
    let parsed: Value = serde_json::from_str(&j.to_string()).unwrap();

    assert_eq!(parsed["schema"], "symhow.not-found/1");
    check_top_level_keys(&parsed, &["symhow_version", "schema", "query", "suggestions"]);
    assert!(parsed["suggestions"].is_array());
    check_kind_fields_are_strings(&parsed);
}

// TODO(parity): port of the "json_shape symhow.index-summary/1 is emitted
// by the real symhow::index::runIndex() --json path" TEST_CASE is skipped:
// it requires running the actual indexer (symhow::index::runIndex with the
// fixture compile_commands.json, capturing the process's stdout), which
// the Rust index layer's fixture-build pipeline does not support here.

#[test]
fn json_shape_determinism_repeated_in_process_calls_dump_byte_for_byte_identical_graph_bases() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let engine = QueryEngine::new(&db).unwrap();
    let writer = JsonWriter::new(&db, &engine, "test-version");

    let square = resolve_one(&db, "fixture::Square");
    let ctx = QueryContext {
        verb: "bases".to_string(),
        db: fixture_db_path(),
        symbol_or_text_or_path: "fixture::Square".to_string(),
        symbol_ids: vec![square],
        depth: 0,
        edge_kinds: vec![edge_kind::INHERITANCE],
        ..QueryContext::default()
    };

    let r1 = engine.bases(&[square], 0, 0, 0).unwrap();
    let r2 = engine.bases(&[square], 0, 0, 0).unwrap();
    let dump1 = writer.graph(&ctx, &r1, /*elapsed_ms=*/ 0, None).unwrap().to_string();
    let dump2 = writer.graph(&ctx, &r2, /*elapsed_ms=*/ 0, None).unwrap().to_string();
    assert_eq!(dump1, dump2);
}

#[test]
fn json_shape_determinism_repeated_in_process_calls_dump_byte_for_byte_identical_search() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let engine = QueryEngine::new(&db).unwrap();
    let writer = JsonWriter::new(&db, &engine, "test-version");

    let ctx = QueryContext {
        verb: "search".to_string(),
        db: fixture_db_path(),
        symbol_or_text_or_path: "sqr".to_string(),
        limit: 10,
        ..QueryContext::default()
    };

    let h1 = engine.search("sqr", None, 10).unwrap();
    let h2 = engine.search("sqr", None, 10).unwrap();
    let dump1 = writer.search(&ctx, &h1, /*elapsed_ms=*/ 0).unwrap().to_string();
    let dump2 = writer.search(&ctx, &h2, /*elapsed_ms=*/ 0).unwrap().to_string();
    assert_eq!(dump1, dump2);
}

#[test]
fn json_shape_determinism_repeated_in_process_calls_dump_byte_for_byte_identical_stats() {
    let db = Database::open(fixture_db_path(), 0).unwrap();
    let engine = QueryEngine::new(&db).unwrap();
    let writer = JsonWriter::new(&db, &engine, "test-version");

    let ctx = QueryContext {
        verb: "stats".to_string(),
        db: fixture_db_path(),
        ..QueryContext::default()
    };

    let s1 = engine.stats(100).unwrap();
    let s2 = engine.stats(100).unwrap();
    let dump1 = writer.stats(&ctx, &s1).unwrap().to_string();
    let dump2 = writer.stats(&ctx, &s2).unwrap().to_string();
    assert_eq!(dump1, dump2);
}
