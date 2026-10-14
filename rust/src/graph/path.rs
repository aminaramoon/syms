//! Weighted shortest path over the loaded graph.
//!
//! Implemented with `petgraph::algo::astar` with a zero heuristic — that is
//! exactly Dijkstra, and unlike `petgraph::algo::dijkstra` (distances only)
//! it returns the actual node path. All three weight schemes are synthetic:
//! the schema has no profile/timing/counter data anywhere (§5.5). Negative
//! weights are impossible, so no Bellman-Ford is needed.

use petgraph::algo::astar;
use petgraph::visit::EdgeRef;

use crate::common::{Error, Result};
use crate::db::enums::edge_kind;

use super::loader::CallGraph;

/// Edge-weight scheme for [`shortest_path`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PathWeight {
    /// Uniform 1 per edge — hop count. The default.
    #[default]
    Hops,
    /// Edge weight = the edge's reference-site count (the §4.2 occurrence
    /// count; call sites for CALL edges, minimum 1).
    Calls,
    /// Edge weight = the callee's body line count (the §4.6 SCOPE span,
    /// minimum 1). Requires the graph to be loaded with
    /// `GraphLoadOptions::with_body_lines = true`.
    Lines,
}

/// A shortest path between two participants of the loaded graph.
#[derive(Debug, Clone, PartialEq)]
pub struct WeightedPath {
    /// Source DB node id.
    pub source: i64,
    /// Target DB node id.
    pub target: i64,
    /// The scheme the cost was computed with.
    pub scheme: PathWeight,
    /// Total path cost under `scheme`.
    pub total_cost: f64,
    /// Node ids along the path, `source ..= target` in walk order.
    pub nodes: Vec<i64>,
    /// `edge.id` of the edge `nodes[i] -> nodes[i+1]` (the CALL edge is
    /// preferred over a parallel USAGE edge when both exist between a
    /// consecutive pair).
    pub edge_ids: Vec<i64>,
}

/// Weighted shortest path from `source` to `target` over `graph`'s loaded
/// edge set, with edge costs per `scheme`. Returns `Ok(None)` when the
/// target is not reachable from the source; errors when either endpoint is
/// not a graph participant (or `Lines` is requested without body-line
/// sizes loaded).
pub fn shortest_path(
    graph: &CallGraph,
    source: i64,
    target: i64,
    scheme: PathWeight,
) -> Result<Option<WeightedPath>> {
    let start = graph.index(source).ok_or_else(|| {
        Error::new(format!(
            "shortest_path: source node {} is not a participant of the loaded graph",
            source
        ))
    })?;
    let goal = graph.index(target).ok_or_else(|| {
        Error::new(format!(
            "shortest_path: target node {} is not a participant of the loaded graph",
            target
        ))
    })?;

    if scheme == PathWeight::Lines && !graph.options.with_body_lines {
        return Err(Error::new(
            "shortest_path: PathWeight::Lines needs body-line sizes; load the graph with GraphLoadOptions::with_body_lines = true",
        ));
    }

    // Zero heuristic => plain Dijkstra; astar returns the node path, which
    // `dijkstra` (distances only) does not.
    let Some((cost, path_indices)) = astar(
        &graph.graph,
        start,
        |index| index == goal,
        |edge| match scheme {
            PathWeight::Hops => 1.0,
            PathWeight::Calls => edge.weight().call_sites.max(1) as f64,
            PathWeight::Lines => {
                let callee_id = graph.graph[edge.target()];
                let lines = graph
                    .nodes
                    .get(&callee_id)
                    .and_then(|node| node.body_lines)
                    .unwrap_or(1);
                lines.max(1) as f64
            }
        },
        |_| 0.0,
    ) else {
        return Ok(None);
    };

    let nodes: Vec<i64> = path_indices
        .iter()
        .map(|&index| graph.graph[index])
        .collect();
    let mut edge_ids: Vec<i64> = Vec::with_capacity(path_indices.len().saturating_sub(1));
    for pair in path_indices.windows(2) {
        // Recover the edge between the consecutive nodes (unique per
        // (source, target, type); prefer CALL over a parallel USAGE edge).
        let mut best: Option<i64> = None;
        for edge in graph.graph.edges(pair[0]) {
            if edge.target() == pair[1] {
                let prefer = match best {
                    None => true,
                    Some(_) => edge.weight().kind == edge_kind::CALL,
                };
                if prefer {
                    best = Some(edge.weight().edge_id);
                }
            }
        }
        edge_ids.push(best.unwrap_or(0)); // unreachable: the path used this edge
    }

    Ok(Some(WeightedPath {
        source,
        target,
        scheme,
        total_cost: cost,
        nodes,
        edge_ids,
    }))
}
