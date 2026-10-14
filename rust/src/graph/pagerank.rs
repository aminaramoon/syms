//! Personalized PageRank over the loaded graph (power iteration).
//!
//! petgraph's built-in `page_rank` is the standard uniform-teleport variant
//! and cannot be personalized, so this is a small hand-rolled power
//! iteration over the loaded edge set with uniform transition probabilities
//! (1/out-degree — call-site multiplicity is deliberately NOT used here):
//!
//! ```text
//! PR[v] = (1-d)·p[v] + d·( Σ_{u→v} PR[u]/outdeg(u) + dangling·p[v] )
//! ```
//!
//! where `d` is the damping factor (teleport probability 1-d), `p` is the
//! personalization vector — uniform over the seed entry points — and
//! `dangling` is the total rank mass on nodes with no outgoing edges,
//! redistributed along `p` so the mass sums to 1 every iteration.

use std::collections::BTreeSet;

use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use crate::common::{Error, Result};

use super::loader::CallGraph;
use super::render::{ranked_scores, ScoredNode};

/// Parameters for [`personalized_pagerank`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageRankParams {
    /// Damping factor `d` in [0, 1] (teleport probability = 1 - d).
    /// Default 0.85.
    pub damping: f64,
    /// Stop once the L1 change between consecutive iterations drops below
    /// this. 0 runs exactly `max_iterations` rounds. Default 1e-6.
    pub tolerance: f64,
    /// Hard iteration cap (>= 1). Default 100.
    pub max_iterations: usize,
}

impl Default for PageRankParams {
    fn default() -> Self {
        PageRankParams {
            damping: 0.85,
            tolerance: 1e-6,
            max_iterations: 100,
        }
    }
}

/// Personalized PageRank over `graph`'s loaded edge set, seeded on the
/// entry-point node ids `seeds` (as returned by
/// [`super::resolve_entry_points`]; every seed must be a graph participant).
/// The personalization vector is uniform over the distinct seeds, so the
/// result mass concentrates around the entry points' reachable callees.
/// Returns every participant ranked by score descending (ties broken by
/// ascending DB node id); scores sum to ~1.
pub fn personalized_pagerank(
    graph: &CallGraph,
    seeds: &[i64],
    params: &PageRankParams,
) -> Result<Vec<ScoredNode>> {
    let digraph = &graph.graph;
    let node_count = digraph.node_count();
    if node_count == 0 {
        return Err(Error::new(
            "personalized_pagerank: the loaded graph is empty",
        ));
    }
    if !(0.0..=1.0).contains(&params.damping) {
        return Err(Error::new(format!(
            "personalized_pagerank: damping {} is outside [0, 1]",
            params.damping
        )));
    }
    if params.tolerance < 0.0 {
        return Err(Error::new(format!(
            "personalized_pagerank: tolerance {} is negative",
            params.tolerance
        )));
    }
    if params.max_iterations == 0 {
        return Err(Error::new(
            "personalized_pagerank: max_iterations must be at least 1",
        ));
    }

    let seed_nodes: BTreeSet<NodeIndex> = seeds
        .iter()
        .filter_map(|&id| graph.index_of.get(&id).copied())
        .collect();
    if seed_nodes.is_empty() {
        return Err(Error::new(
            "personalized_pagerank: no seed entry points (none given, and none resolve to graph participants)",
        ));
    }
    let seed_mass = 1.0 / seed_nodes.len() as f64;
    let personalization = |index: usize| -> f64 {
        if seed_nodes.contains(&NodeIndex::new(index)) {
            seed_mass
        } else {
            0.0
        }
    };

    // Node indices of a petgraph DiGraph are dense 0..node_count-1.
    let out_degree: Vec<usize> = (0..node_count)
        .map(|i| digraph.neighbors(NodeIndex::new(i)).count())
        .collect();

    let damping = params.damping;
    let mut rank = vec![0.0f64; node_count];
    let mut next = vec![0.0f64; node_count];
    for &seed in &seed_nodes {
        rank[seed.index()] = seed_mass;
    }

    for _ in 0..params.max_iterations {
        // Rank mass sitting on dangling nodes (no outgoing edges), which
        // would otherwise leak; redistributed along the personalization.
        let mut dangling = 0.0;
        for (&degree, &mass) in out_degree.iter().zip(rank.iter()) {
            if degree == 0 {
                dangling += mass;
            }
        }

        for value in next.iter_mut() {
            *value = 0.0;
        }
        for reference in digraph.edge_references() {
            let source = reference.source().index();
            let target = reference.target().index();
            next[target] += rank[source] / out_degree[source] as f64;
        }
        for (i, value) in next.iter_mut().enumerate() {
            let p = personalization(i);
            *value = (1.0 - damping) * p + damping * (*value + dangling * p);
        }

        let change: f64 = next
            .iter()
            .zip(rank.iter())
            .map(|(updated, previous)| (updated - previous).abs())
            .sum();
        std::mem::swap(&mut rank, &mut next);
        if change < params.tolerance {
            break;
        }
    }

    Ok(ranked_scores(graph, &rank))
}
