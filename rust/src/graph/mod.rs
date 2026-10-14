//! Graph analysis over the indexed call/usage graph, backed by `petgraph`.
//!
//! The layer is built directly on the `.srctrldb` database read through the
//! existing `db` module; `docs/GRAPH_SCHEMA_NOTES.md` is the schema study
//! this whole module follows (SQL, enum codes, per-algorithm verdicts, and
//! the fixture ground truth). The analyses it exposes:
//!
//! - **call-graph loading** — [`load_call_graph`] (CALL edges §4.2,
//!   optionally USAGE edges + source lines §4.4 and body-line sizes §4.6),
//!   plus [`resolve_entry_points`] for seed resolution;
//! - **betweenness centrality** — [`betweenness`] (hand-rolled Brandes);
//! - **personalized PageRank** — [`personalized_pagerank`] (power iteration
//!   seeded on entry points, damping default 0.85);
//! - **dominator tree** — [`dominator_tree`] (call-graph dominance from one
//!   entry point, via `petgraph::algo::dominators::simple_fast`);
//! - **backward slicing** — [`backward_slice`] (reverse closure over
//!   USAGE + CALL edges, function granularity, with per-edge source lines);
//! - **weighted shortest path** — [`shortest_path`] (Dijkstra via
//!   `petgraph::algo::astar` with the [`PathWeight`] schemes).
//!
//! Storage shape: a `petgraph::graph::DiGraph<i64, GraphEdgeData>` whose
//! node weight is the DB node id, plus a `HashMap<i64, NodeIndex>` side map
//! — all results speak DB node ids so they stay compatible with the
//! `symhow.graph`-style JSON output. Loading is isolated in [`loader`] so a
//! CSR swap later stays cheap. This module is additive: it changes no
//! existing module's public API.
//!
//! Typical use:
//!
//! ```no_run
//! use symhow::db::Database;
//! use symhow::graph::{self, GraphLoadOptions, PathWeight};
//!
//! # fn main() -> symhow::common::Result<()> {
//! let db = Database::open("build/fixture.srctrldb", 0)?;
//! let graph = graph::load_call_graph(&db, &GraphLoadOptions::default())?;
//!
//! let seeds = graph::resolve_entry_points(&db, &graph, &["main".to_string()])?;
//! let ranked = graph::personalized_pagerank(&graph, &seeds, &Default::default())?;
//! for node in ranked.iter().take(5) {
//!     println!("{}\t{}", node.score, node.display_name);
//! }
//!
//! if let Some(path) = graph::shortest_path(&graph, ranked[0].node_id, seeds[0], PathWeight::Hops)? {
//!     println!("{} hops", path.total_cost);
//! }
//! # Ok(())
//! # }
//! ```

pub mod centrality;
pub mod dominators;
pub mod loader;
pub mod pagerank;
pub mod path;
pub mod render;
pub mod slicing;

pub use centrality::betweenness;
pub use dominators::{dominator_tree, DominatorTree};
pub use loader::{
    load_call_graph, resolve_entry_points, CallGraph, EdgeSite, GraphEdgeData, GraphLoadOptions,
    GraphNodeData,
};
pub use pagerank::{personalized_pagerank, PageRankParams};
pub use path::{shortest_path, PathWeight, WeightedPath};
pub use render::{call_graph_dot, hydrate_node_refs, induced_subgraph_dot, NodeRef, ScoredNode};
pub use slicing::{backward_slice, BackwardSlice, SliceEdge};
