//! Byte-exact formatting tests for the TextWriter port
//! (src/out/TextWriter.cpp). These use synthetic result structs only -- no
//! database needed -- and lock the exact output bytes (indentation,
//! separators, setw(4) score alignment, kind names, empty-result messages).

use std::collections::HashMap;

use symhow::db::enums::{edge_kind, location_kind, node_kind};
use symhow::out::{
    write_bfs_result, write_candidate_table, write_definition, write_file_result,
    write_references, write_search_hits, write_stats, CandidateRow, NodeNoteText,
};
use symhow::query::{
    BfsEdge, BfsNode, BfsResult, DefinitionResult, FileLocation, FileOccurrence, FileResult,
    KindCount, ReferenceHit, ReferencesResult, SearchHit, StatsErrorRow, StatsResult,
};

// The fixture's known-good serialization of "fixture::Square"
// (see tests/query_fixture.rs) -- decode_serialized_name turns this into
// display_name()/qualified_name() == "fixture::Square".
const SQUARE_SERIALIZED: &str = "::\tmfixture\ts\tp\tnSquare\ts\tp";

fn loc(file_path: &str, start_line: i32, start_column: i32, end_line: i32, end_column: i32, kind: i32) -> FileLocation {
    FileLocation {
        file_id: 3,
        file_path: file_path.to_string(),
        start_line,
        start_column,
        end_line,
        end_column,
        location_kind: kind,
    }
}

#[test]
fn text_write_search_hits_empty_prints_no_matches() {
    assert_eq!(write_search_hits(&[]), "(no matches)\n");
}

#[test]
fn text_write_search_hits_formats_score_width_kind_names_and_not_indexed_tag() {
    let hits = vec![
        SearchHit {
            node_id: 1,
            kind: node_kind::CLASS,
            display_name: "fixture::Square".to_string(),
            qualified_name: "fixture::Square".to_string(),
            score: 47,
            indexed: true,
            matched_indices: vec![0, 1, 2],
        },
        SearchHit {
            node_id: 2,
            kind: node_kind::FUNCTION,
            display_name: "do_thing".to_string(),
            qualified_name: "do_thing".to_string(),
            score: 5,
            indexed: false,
            matched_indices: vec![],
        },
    ];
    assert_eq!(
        write_search_hits(&hits),
        "  47  [CLASS]  fixture::Square  (fixture::Square)\n\
         \x20  5  [FUNCTION]  do_thing  (do_thing)  [not indexed]\n"
    );
}

#[test]
fn text_write_bfs_result_renders_tree_locations_truncation_and_edge_list() {
    let result = BfsResult {
        nodes: vec![
            BfsNode {
                id: 42,
                kind: node_kind::CLASS,
                serialized_name: SQUARE_SERIALIZED.to_string(),
                depth: 0,
            },
            BfsNode {
                id: 7,
                kind: node_kind::STRUCT,
                serialized_name: SQUARE_SERIALIZED.to_string(),
                depth: 1,
            },
        ],
        edges: vec![BfsEdge {
            id: 10,
            kind: edge_kind::MEMBER,
            source: 42,
            target: 7,
        }],
        truncated: true,
    };
    let mut locations = HashMap::new();
    locations.insert(42_i64, "fixture/shapes.hpp:31".to_string());

    let out = write_bfs_result(&result, &locations, true, None, false);
    assert_eq!(
        out,
        "[CLASS] fixture::Square (#42)  (fixture/shapes.hpp:31)\n\
         \x20 [STRUCT] fixture::Square (#7)\n\
         1 edge(s) [truncated: --max-nodes reached]\n\
         ... truncated at 2 nodes\n\
         \x20 #42 --[MEMBER]--> #7\n"
    );
}

#[test]
fn text_write_bfs_result_empty_prints_no_nodes_and_edge_count() {
    let out = write_bfs_result(&BfsResult::default(), &HashMap::new(), false, None, false);
    assert_eq!(out, "(no nodes)\n0 edge(s)\n");
}

#[test]
fn text_write_bfs_result_renders_brief_note_with_stale_and_unfilled_tags() {
    let result = BfsResult {
        nodes: vec![BfsNode {
            id: 7,
            kind: node_kind::METHOD,
            serialized_name: SQUARE_SERIALIZED.to_string(),
            depth: 1,
        }],
        edges: vec![],
        truncated: false,
    };
    let mut notes: HashMap<i64, NodeNoteText> = HashMap::new();
    notes.insert(
        7,
        NodeNoteText {
            brief: "ok".to_string(),
            filled: false,
            stale: true,
            full_markdown: String::new(),
        },
    );
    // Node absent from the notes map prints nothing extra; node 7 is at
    // depth 1 (2-space indent) so the note line is indented 2 + 4 spaces.
    let out = write_bfs_result(&result, &HashMap::new(), false, Some(&notes), false);
    assert_eq!(
        out,
        "  [METHOD] fixture::Square (#7)\n\
         \x20     brief: ok [stale]\n\
         0 edge(s)\n"
    );
}

#[test]
fn text_write_bfs_result_renders_full_markdown_note_when_notes_full() {
    let result = BfsResult {
        nodes: vec![BfsNode {
            id: 42,
            kind: node_kind::CLASS,
            serialized_name: SQUARE_SERIALIZED.to_string(),
            depth: 0,
        }],
        edges: vec![],
        truncated: false,
    };
    let mut notes: HashMap<i64, NodeNoteText> = HashMap::new();
    notes.insert(
        42,
        NodeNoteText {
            brief: "b".to_string(),
            filled: true,
            stale: false,
            full_markdown: "## fixture::Square\n\nwhat\n".to_string(),
        },
    );
    let out = write_bfs_result(&result, &HashMap::new(), false, Some(&notes), true);
    assert_eq!(
        out,
        "[CLASS] fixture::Square (#42)\n\
         \x20   note:\n\
         \x20     ## fixture::Square\n\
         \x20     \n\
         \x20     what\n\
         0 edge(s)\n"
    );
}

#[test]
fn text_write_references_empty_prints_no_references() {
    assert_eq!(write_references(&ReferencesResult::default()), "(no references)\n");
}

#[test]
fn text_write_references_formats_edge_and_declaration_lines() {
    let result = ReferencesResult {
        hits: vec![
            ReferenceHit {
                has_edge: true,
                edge_id: 9,
                edge_kind: edge_kind::CALL,
                context_node_id: 5,
                target_node_id: 3,
                location: loc("a.cpp", 10, 4, 10, 9, location_kind::TOKEN),
            },
            ReferenceHit {
                has_edge: false,
                edge_id: 0,
                edge_kind: 0,
                context_node_id: 0,
                target_node_id: 3,
                location: loc("b.cpp", 12, 2, 12, 6, location_kind::SCOPE),
            },
        ],
    };
    assert_eq!(
        write_references(&result),
        "a.cpp:10:4  [TOKEN]  CALL  #5 -> #3\n\
         b.cpp:12:2  [SCOPE]  declaration  #3\n"
    );
}

#[test]
fn text_write_definition_empty_prints_no_definition_found() {
    assert_eq!(
        write_definition(&DefinitionResult::default()),
        "(no definition found)\n"
    );
}

#[test]
fn text_write_definition_formats_location_spans() {
    let result = DefinitionResult {
        locations: vec![loc("x.hpp", 31, 1, 41, 2, location_kind::SCOPE)],
    };
    assert_eq!(write_definition(&result), "x.hpp:31:1-41:2  [SCOPE]\n");
}

#[test]
fn text_write_file_result_formats_metadata_contents_and_occurrences() {
    let result = FileResult {
        file_id: 3,
        path: "/abs/shapes.hpp".to_string(),
        language: "cpp".to_string(),
        indexed: true,
        complete: false,
        line_count: 43,
        has_contents: true,
        contents: "int x;".to_string(),
        occurrences: vec![
            FileOccurrence {
                element_id: 8,
                element_kind: node_kind::NAMESPACE,
                element_name: "fixture".to_string(),
                location: loc("/abs/shapes.hpp", 1, 5, 1, 12, location_kind::TOKEN),
            },
            FileOccurrence {
                element_id: 99,
                element_kind: 0,
                element_name: String::new(),
                location: loc("/abs/shapes.hpp", 2, 7, 2, 11, location_kind::LOCAL_SYMBOL),
            },
        ],
    };
    assert_eq!(
        write_file_result(&result),
        "path: /abs/shapes.hpp\n\
         id: 3\n\
         language: cpp\n\
         indexed: true\n\
         complete: false\n\
         line_count: 43\n\
         --- contents ---\n\
         int x;\n\
         --- occurrences (2) ---\n\
         1:5  [NAMESPACE] fixture\n\
         2:7  (non-node element #99)\n"
    );
}

#[test]
fn text_write_stats_formats_counts_by_kind_and_error_table() {
    let result = StatsResult {
        storage_version: 25,
        node_count: 3,
        edge_count: 2,
        file_count: 1,
        symbol_count: 3,
        occurrence_count: 4,
        source_location_count: 5,
        local_symbol_count: 1,
        error_count: 1,
        nodes_by_kind: vec![KindCount {
            kind: node_kind::CLASS,
            count: 1,
        }],
        edges_by_kind: vec![KindCount {
            kind: edge_kind::CALL,
            count: 2,
        }],
        errors: vec![
            StatsErrorRow {
                id: 9,
                message: "bad parse".to_string(),
                fatal: true,
                translation_unit: "x.cu".to_string(),
            },
            StatsErrorRow {
                id: 11,
                message: "warn".to_string(),
                fatal: false,
                translation_unit: "y.cu".to_string(),
            },
        ],
    };
    assert_eq!(
        write_stats(&result),
        "storage_version: 25\n\
         nodes: 3\n\
         edges: 2\n\
         files: 1\n\
         symbols: 3\n\
         occurrences: 4\n\
         source_locations: 5\n\
         local_symbols: 1\n\
         errors: 1\n\
         --- nodes by kind ---\n\
         \x20 CLASS: 1\n\
         --- edges by kind ---\n\
         \x20 CALL: 2\n\
         --- errors (2 shown) ---\n\
         \x20 #9 [FATAL] x.cu: bad parse\n\
         \x20 #11 y.cu: warn\n"
    );
}

#[test]
fn text_write_candidate_table_formats_rows_with_optional_location() {
    let rows = vec![
        CandidateRow {
            id: 1,
            kind: "CLASS".to_string(),
            name: "Square".to_string(),
            location: "shapes.hpp:31".to_string(),
        },
        CandidateRow {
            id: 2,
            kind: "METHOD".to_string(),
            name: "area".to_string(),
            location: String::new(),
        },
    ];
    assert_eq!(
        write_candidate_table(&rows),
        "  #1  [CLASS]  Square  (shapes.hpp:31)\n  #2  [METHOD]  area\n"
    );
}
