//! Output layer: text and JSON writers for query results. Mirrors
//! `symhow::out` (`src/out/TextWriter.*`, `src/out/JsonWriter.*`, and the
//! placeholder `src/out/Out.*`).

pub mod json_writer;
pub mod out;
pub mod text_writer;

pub use json_writer::{decompose_edge_kind_mask, JsonWriter, QueryContext};
pub use text_writer::{
    write_bfs_result, write_candidate_table, write_definition, write_file_result, write_references,
    write_search_hits, write_stats, CandidateRow, NodeNoteText,
};
