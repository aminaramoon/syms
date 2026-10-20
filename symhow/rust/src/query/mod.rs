//! Query layer: graph traversal, fuzzy matching, symbol resolution, and the
//! `symhow query` verbs. Mirrors `symhow::query`.

mod bfs;
mod fuzzy_matcher;
mod query_engine;
mod results;
mod symbol_resolver;

pub use bfs::{run_bfs, BfsEdge, BfsNode, BfsParams, BfsResult};
pub use fuzzy_matcher::{fuzzy_match, FuzzyHit};
pub use query_engine::QueryEngine;
pub use results::{
    DefinitionResult, FileLocation, FileOccurrence, FileResult, KindCount, ReferenceHit,
    ReferencesResult, SearchHit, StatsErrorRow, StatsResult,
};
pub use symbol_resolver::{
    resolve_file_path, resolve_symbol, ResolveOutcome, ResolvedSymbol, ResolveResult,
};