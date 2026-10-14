//! Backward program slicing at function granularity.
//!
//! The slice of a criterion node is its reverse-transitive closure over the
//! loaded USAGE + CALL edges (petgraph `Reversed` view + `Dfs`), presented
//! as the closure's node set plus the induced dependence edges between
//! members, each carrying its reference-site source lines (§4.4) so the CLI
//! can show lines later.
//!
//! Approximate by design (§5.4): USAGE does not distinguish reads from
//! writes, there is no statement-level PDG, no aliasing analysis, and no
//! intra-procedural dependences — local variables never become edge
//! endpoints in this schema at all. Load the graph with
//! `GraphLoadOptions::include_usage = true` or the slice degrades to CALL
//! edges only.

use std::collections::BTreeSet;

use petgraph::graph::NodeIndex;
use petgraph::visit::{Dfs, EdgeRef, Reversed};

use crate::common::{Error, Result};

use super::loader::{CallGraph, EdgeSite};

/// One dependence edge of a backward slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceEdge {
    /// `edge.id`.
    pub edge_id: i64,
    /// Source DB node id (the depending side: caller / using function).
    pub source: i64,
    /// Target DB node id (the depended-on side: callee / accessed symbol).
    pub target: i64,
    /// EdgeKind bit: `edge_kind::CALL` (8) or `edge_kind::USAGE` (4).
    pub kind: i32,
    /// Reference-site count (call sites for CALL, usage sites for USAGE).
    pub call_sites: u32,
    /// The reference sites (source lines) of this edge.
    pub sites: Vec<EdgeSite>,
}

/// The backward slice of one criterion node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackwardSlice {
    /// The criterion node's DB id.
    pub criterion: i64,
    /// Slice member node ids (the criterion included), sorted ascending.
    pub nodes: Vec<i64>,
    /// Dependence edges between slice members (the induced subgraph's edge
    /// set), sorted by (source, target, kind, edge id).
    pub edges: Vec<SliceEdge>,
}

/// Computes the backward slice of the criterion node id `criterion` (as
/// returned by [`super::resolve_entry_points`] or any other participant
/// lookup). Errors when the criterion is not a graph participant.
pub fn backward_slice(graph: &CallGraph, criterion: i64) -> Result<BackwardSlice> {
    let start = graph.index(criterion).ok_or_else(|| {
        Error::new(format!(
            "backward_slice: criterion node {} is not a participant of the loaded graph",
            criterion
        ))
    })?;

    // Reverse closure over USAGE + CALL edges.
    let reversed = Reversed(&graph.graph);
    let mut dfs = Dfs::new(reversed, start);
    let mut closure: BTreeSet<NodeIndex> = BTreeSet::new();
    while let Some(node) = dfs.next(reversed) {
        closure.insert(node);
    }

    let mut nodes: Vec<i64> = closure.iter().map(|&index| graph.graph[index]).collect();
    nodes.sort_unstable();

    // Every edge whose both endpoints are slice members is a genuine
    // dependence between two slice members (each endpoint reaches the
    // criterion backwards), so the induced edge set is the slice's edges.
    let mut edges: Vec<SliceEdge> = Vec::new();
    for reference in graph.graph.edge_references() {
        if closure.contains(&reference.source()) && closure.contains(&reference.target()) {
            let weight = reference.weight();
            edges.push(SliceEdge {
                edge_id: weight.edge_id,
                source: graph.graph[reference.source()],
                target: graph.graph[reference.target()],
                kind: weight.kind,
                call_sites: weight.call_sites,
                sites: weight.sites.clone(),
            });
        }
    }
    edges.sort_by(|a, b| {
        (a.source, a.target, a.kind, a.edge_id).cmp(&(b.source, b.target, b.kind, b.edge_id))
    });

    Ok(BackwardSlice {
        criterion,
        nodes,
        edges,
    })
}
