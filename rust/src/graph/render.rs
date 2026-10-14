//! Result types and DOT rendering for the graph module.
//!
//! The result structs are plain data ready for the `src/out/` writers:
//! ranked scores carry the decoded display name and node kind so a writer
//! needs no further DB access, and the id-only results (slice, path,
//! dominator tree) can be hydrated with [`hydrate_node_refs`] or directly
//! from [`CallGraph::nodes`].
//!
//! DOT output goes through `petgraph::dot::Dot` with custom label
//! attributes: node labels are the decoded display name plus the node kind,
//! edge labels are the edge kind plus the reference-site count when > 1.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;

use petgraph::dot::{Config, Dot};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::{EdgeRef, IntoNodeIdentifiers};

use crate::common::{Error, Result};
use crate::db::enums::edge_kind_to_string;

use super::loader::{participant_kind_name, CallGraph, GraphEdgeData};

/// A ranked score for one node (betweenness centrality, PageRank).
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredNode {
    /// DB node id.
    pub node_id: i64,
    /// `node.type` NodeKind bit mask.
    pub kind: i32,
    /// Decoded display name (TU-local disambiguator stripped).
    pub display_name: String,
    /// The score: the raw Brandes dependency sum for betweenness (not
    /// divided by 2, not normalized; for a directed graph with n > 2 nodes
    /// the normalized value is `score / ((n-1)(n-2))`), or the PageRank
    /// probability mass (all scores sum to ~1).
    pub score: f64,
}

/// A node id hydrated with the metadata the out layer needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRef {
    /// DB node id.
    pub node_id: i64,
    /// `node.type` NodeKind bit mask.
    pub kind: i32,
    /// Decoded display name (TU-local disambiguator stripped).
    pub display_name: String,
}

/// Hydrates node ids (in the given order) into [`NodeRef`]s. Ids that are
/// not participants of `graph` come back with kind 0 and an empty name.
pub fn hydrate_node_refs(graph: &CallGraph, ids: &[i64]) -> Vec<NodeRef> {
    ids.iter()
        .map(|&id| match graph.nodes.get(&id) {
            Some(node) => NodeRef {
                node_id: id,
                kind: node.kind,
                display_name: node.display_name.clone(),
            },
            None => NodeRef {
                node_id: id,
                kind: 0,
                display_name: String::new(),
            },
        })
        .collect()
}

/// Ranked [`ScoredNode`]s from a per-`NodeIndex` score vector (indexed like
/// petgraph node indices), sorted by score descending then node id
/// ascending. Shared by the centrality algorithms.
pub(crate) fn ranked_scores(graph: &CallGraph, scores: &[f64]) -> Vec<ScoredNode> {
    let mut out: Vec<ScoredNode> = Vec::with_capacity(graph.node_count());
    for index in graph.graph.node_identifiers() {
        let id = graph.graph[index];
        let data = graph.nodes.get(&id);
        out.push(ScoredNode {
            node_id: id,
            kind: data.map_or(0, |node| node.kind),
            display_name: data.map_or(String::new(), |node| node.display_name.clone()),
            score: scores[index.index()],
        });
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.node_id.cmp(&b.node_id))
    });
    out
}

/// DOT (Graphviz) rendering of the whole loaded graph. Node labels are
/// `"<display name> (<KIND>)"`; edge labels are `"<KIND>"` with a
/// `" x<N>"` suffix when the edge has more than one reference site.
pub fn call_graph_dot(graph: &CallGraph) -> String {
    dot_string(&graph.graph, graph)
}

/// DOT rendering of the subgraph induced by `node_ids` (e.g. a backward
/// slice's member set). Edges between non-listed nodes are not included;
/// duplicate ids are harmless. Errors when an id is not a participant of
/// `graph`.
pub fn induced_subgraph_dot(graph: &CallGraph, node_ids: &[i64]) -> Result<String> {
    let mut sub_index: HashMap<i64, NodeIndex> = HashMap::with_capacity(node_ids.len());
    let mut sub = DiGraph::with_capacity(node_ids.len(), 0);
    let unique: BTreeSet<i64> = node_ids.iter().copied().collect();
    for &id in &unique {
        if !graph.index_of.contains_key(&id) {
            return Err(Error::new(format!(
                "induced_subgraph_dot: node {} is not a participant of the loaded graph",
                id
            )));
        }
        let index = sub.add_node(id);
        sub_index.insert(id, index);
    }
    for reference in graph.graph.edge_references() {
        let source_id = graph.graph[reference.source()];
        let target_id = graph.graph[reference.target()];
        if let (Some(&source), Some(&target)) =
            (sub_index.get(&source_id), sub_index.get(&target_id))
        {
            sub.add_edge(source, target, reference.weight().clone());
        }
    }
    Ok(dot_string(&sub, graph))
}

/// Shared DOT emitter. `petgraph::dot::Dot` renders through its `Debug`
/// impl (its `Display` impl additionally requires `Display` on the weight
/// types, which `GraphEdgeData` deliberately does not implement); with
/// `NodeNoLabel`/`EdgeNoLabel` both formatter paths emit the same DOT.
fn dot_string(graph: &DiGraph<i64, GraphEdgeData>, meta: &CallGraph) -> String {
    let edge_attr = |_, edge: petgraph::graph::EdgeReference<'_, GraphEdgeData>| {
        let weight = edge.weight();
        let mut label = edge_kind_to_string(weight.kind);
        if weight.call_sites > 1 {
            let _ = write!(label, " x{}", weight.call_sites);
        }
        format!(r#"label = "{}""#, dot_escape(&label))
    };
    let node_attr = |_, node: (NodeIndex, &i64)| {
        let id = *node.1;
        let (kind, name) = match meta.nodes.get(&id) {
            Some(node) => (node.kind, node.display_name.as_str()),
            None => (0, ""),
        };
        format!(
            r#"label = "{}""#,
            dot_escape(&format!("{} ({})", name, participant_kind_name(kind)))
        )
    };
    let dot = Dot::with_attr_getters(
        graph,
        &[Config::NodeNoLabel, Config::EdgeNoLabel],
        &edge_attr,
        &node_attr,
    );
    format!("{:?}", dot)
}

/// Escapes a string for a DOT quoted attribute value (backslash, double
/// quote, newline).
fn dot_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            other => out.push(other),
        }
    }
    out
}
