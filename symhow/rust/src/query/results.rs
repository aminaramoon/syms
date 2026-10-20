//! Value-type result structs for every `symhow::query::QueryEngine` verb.
//! Mirrors `symhow::query::Results` — plain aggregates, nothing that isn't
//! trivially serializable.

/// One source span (mirrors `symhow::query::FileLocation`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileLocation {
    pub file_id: i64,
    pub file_path: String,
    pub start_line: i32,
    pub start_column: i32,
    pub end_line: i32,
    pub end_column: i32,
    pub location_kind: i32, // symhow::db::LocationKind value
}

/// One row of `references(symbol_id)` (mirrors `symhow::query::ReferenceHit`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReferenceHit {
    pub has_edge: bool,
    pub edge_id: i64,
    pub edge_kind: i32,
    pub context_node_id: i64, // the referencing symbol (edge.source_node_id); 0 if !has_edge
    pub target_node_id: i64,  // the symbol references() was called on
    pub location: FileLocation,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReferencesResult {
    pub hits: Vec<ReferenceHit>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DefinitionResult {
    pub locations: Vec<FileLocation>,
}

/// One fuzzy-search hit (mirrors `symhow::query::SearchHit`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub node_id: i64,
    pub kind: i32,
    pub display_name: String,
    pub qualified_name: String,
    pub score: i32,
    pub indexed: bool, // false when node.id has no `symbol` row (definition_kind IS NULL)
    pub matched_indices: Vec<i32>,
}

impl Default for SearchHit {
    fn default() -> Self {
        SearchHit {
            node_id: 0,
            kind: 0,
            display_name: String::new(),
            qualified_name: String::new(),
            score: 0,
            indexed: true,
            matched_indices: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileOccurrence {
    pub element_id: i64,
    pub element_kind: i32,
    pub element_name: String, // decoded displayName() of the element, or empty if it's an edge/local-symbol id
    pub location: FileLocation,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileResult {
    pub file_id: i64,
    pub path: String,
    pub language: String,
    pub indexed: bool,
    pub complete: bool,
    pub line_count: i32,

    pub has_contents: bool,
    pub contents: String,

    pub occurrences: Vec<FileOccurrence>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KindCount {
    pub kind: i32,
    pub count: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatsErrorRow {
    pub id: i64,
    pub message: String,
    pub fatal: bool,
    pub translation_unit: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatsResult {
    pub storage_version: i32,

    pub node_count: i64,
    pub edge_count: i64,
    pub file_count: i64,
    pub symbol_count: i64,
    pub occurrence_count: i64,
    pub source_location_count: i64,
    pub local_symbol_count: i64,
    pub error_count: i64,

    pub nodes_by_kind: Vec<KindCount>,
    pub edges_by_kind: Vec<KindCount>,
    pub errors: Vec<StatsErrorRow>,
}