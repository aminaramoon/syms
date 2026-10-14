//! Betweenness centrality (Brandes' algorithm, unweighted).
//!
//! petgraph has no built-in betweenness (checked through 0.8), so this is
//! the standard hand-rolled accumulation (Brandes 2001): for every source,
//! one BFS computes shortest-path distances, path counts (sigma) and
//! predecessor lists; then a reverse-BFS-order dependency accumulation adds
//! up delta into the centrality scores. O(V·E) time, O(V + E) memory.
//!
//! Scores are the raw directed-graph dependency sums — NOT halved (that is
//! the undirected convention) and NOT normalized. The graph is traversed
//! over its loaded edge set (CALL, plus USAGE when loaded with
//! `GraphLoadOptions::include_usage`), with edge weights ignored (hop
//! count 1).

use std::collections::VecDeque;

use petgraph::graph::NodeIndex;
use petgraph::visit::IntoNodeIdentifiers;

use crate::common::Result;

use super::loader::CallGraph;
use super::render::ranked_scores;
use super::render::ScoredNode;

/// Betweenness centrality of every participant of `graph`, ranked by score
/// descending (ties broken by ascending DB node id). Isolated nodes score
/// 0.0 and still appear in the result.
pub fn betweenness(graph: &CallGraph) -> Result<Vec<ScoredNode>> {
    let digraph = &graph.graph;
    let node_count = digraph.node_count();
    if node_count == 0 {
        return Ok(Vec::new());
    }

    let mut scores = vec![0.0f64; node_count];
    let mut sigma = vec![0.0f64; node_count];
    // usize::MAX doubles as "unvisited" (BFS distances never reach it).
    let mut dist = vec![usize::MAX; node_count];
    let mut delta = vec![0.0f64; node_count];
    // Predecessor lists are allocated once and reset per source by walking
    // the previous BFS's visited set (`order`) — O(V·E) total either way,
    // but without O(V) fresh allocations per source.
    let mut preds: Vec<Vec<NodeIndex>> = vec![Vec::new(); node_count];
    let mut order: Vec<NodeIndex> = Vec::with_capacity(node_count);
    let mut queue: VecDeque<NodeIndex> = VecDeque::with_capacity(node_count);

    for source in digraph.node_identifiers() {
        for &visited in &order {
            let i = visited.index();
            sigma[i] = 0.0;
            dist[i] = usize::MAX;
            delta[i] = 0.0;
            preds[i].clear();
        }
        order.clear();

        sigma[source.index()] = 1.0;
        dist[source.index()] = 0;
        queue.push_back(source);
        while let Some(node) = queue.pop_front() {
            order.push(node);
            let next_dist = dist[node.index()] + 1;
            for neighbor in digraph.neighbors(node) {
                let ni = neighbor.index();
                if dist[ni] == usize::MAX {
                    dist[ni] = next_dist;
                    queue.push_back(neighbor);
                }
                if dist[ni] == next_dist {
                    sigma[ni] += sigma[node.index()];
                    preds[ni].push(node);
                }
            }
        }

        for node in order.iter().rev() {
            let ni = node.index();
            let coeff = (1.0 + delta[ni]) / sigma[ni];
            for &predecessor in &preds[ni] {
                delta[predecessor.index()] += sigma[predecessor.index()] * coeff;
            }
            if *node != source {
                scores[ni] += delta[ni];
            }
        }
    }

    Ok(ranked_scores(graph, &scores))
}
