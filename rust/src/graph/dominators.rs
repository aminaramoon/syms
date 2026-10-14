//! Dominator tree over the call graph, rooted at an entry point.
//!
//! Uses `petgraph::algo::dominators::simple_fast` (the schema study
//! confirmed it handles the recursion cycles the fixture contains —
//! `factorial` self-loop, `ping`⇄`pong`). Semantics are **call-graph
//! dominance** — "which functions must have been called on every path from
//! the entry point to X" — NOT control-flow dominance: the schema has no
//! basic blocks or intra-procedural control flow at all (§5.1).

use petgraph::algo::dominators::simple_fast;
use petgraph::visit::IntoNodeIdentifiers;

use crate::common::{Error, Result};

use super::loader::CallGraph;

/// The dominator tree of the call graph rooted at one entry point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DominatorTree {
    /// The root (entry point) DB node id.
    pub root: i64,
    /// `(node, immediate dominator)` pairs for every node reachable from
    /// the root, sorted by node id. The root itself has no entry (it
    /// trivially dominates itself), and neither do nodes unreachable from
    /// the root.
    pub idoms: Vec<(i64, i64)>,
}

/// Builds the dominator tree of `graph`'s loaded edge set rooted at the
/// entry-point node id `root` (as returned by
/// [`super::resolve_entry_points`]). Errors when `root` is not a graph
/// participant.
pub fn dominator_tree(graph: &CallGraph, root: i64) -> Result<DominatorTree> {
    let root_index = graph.index(root).ok_or_else(|| {
        Error::new(format!(
            "dominator_tree: root node {} is not a participant of the loaded graph",
            root
        ))
    })?;

    let dominators = simple_fast(&graph.graph, root_index);
    let mut idoms: Vec<(i64, i64)> = Vec::new();
    for node in graph.graph.node_identifiers() {
        if let Some(idom_index) = dominators.immediate_dominator(node) {
            idoms.push((graph.graph[node], graph.graph[idom_index]));
        }
    }
    idoms.sort_unstable();

    Ok(DominatorTree { root, idoms })
}
