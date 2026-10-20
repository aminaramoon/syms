//! The single graph-traversal primitive every `QueryEngine` graph verb
//! (callers/callees/bases/derived/overrides/overriddenBy/members) is built
//! on. Mirrors `symhow::query::Bfs`: one batched SQL query per BFS level,
//! a global visited set, and an optional node-kind filter with Sourcetrail's
//! "namespace containment hops pass through transparently" behavior.

use std::collections::{HashMap, HashSet};

use rusqlite::types::Value;

use crate::common::Result;
use crate::db::enums::is_namespace_like_node_kind;
use crate::db::Database;

/// Chunk size for the chunked-IN helpers (the C++ `runChunkedIn` default).
pub(crate) const CHUNK_SIZE: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BfsParams {
    pub seeds: Vec<i64>,
    /// Tested via `(edge.type & edge_kind_mask) != 0`.
    pub edge_kind_mask: i32,
    /// true: follow source_node_id -> target_node_id; false: reverse.
    pub forward: bool,
    /// 0 = unlimited.
    pub depth: i32,
    /// 0 = unlimited; caps result.nodes, sets result.truncated when exceeded.
    pub max_nodes: i32,
    /// 0 = no filtering.
    pub node_kind_mask: i32,
}

impl Default for BfsParams {
    fn default() -> Self {
        BfsParams {
            seeds: Vec::new(),
            edge_kind_mask: 0,
            forward: true,
            depth: 0,
            max_nodes: 0,
            node_kind_mask: 0,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BfsNode {
    pub id: i64,
    pub kind: i32,
    pub serialized_name: String,
    /// 0 for seeds, else the BFS level at which the node was first discovered.
    pub depth: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BfsEdge {
    pub id: i64,
    pub kind: i32,
    pub source: i64,
    pub target: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BfsResult {
    /// Every edge discovered during the traversal, REGARDLESS of whether its
    /// far endpoint passed node_kind_mask or the max_nodes cap.
    pub edges: Vec<BfsEdge>,
    /// Every visited node except namespace-like pass-through hops under a
    /// node-kind filter.
    pub nodes: Vec<BfsNode>,
    pub truncated: bool,
}

fn sort_nodes(nodes: &mut [BfsNode]) {
    nodes.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then(a.kind.cmp(&b.kind))
            .then(a.serialized_name.cmp(&b.serialized_name))
            .then(a.id.cmp(&b.id))
    });
}

fn sort_edges(edges: &mut [BfsEdge]) {
    edges.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then(a.source.cmp(&b.source))
            .then(a.target.cmp(&b.target))
            .then(a.id.cmp(&b.id))
    });
}

struct EdgeRow {
    id: i64,
    kind: i32,
    source: i64,
    target: i64,
}

pub fn run_bfs(db: &Database, params: &BfsParams) -> Result<BfsResult> {
    let mut result = BfsResult::default();
    if params.seeds.is_empty() {
        return Ok(result);
    }

    let mut visited: HashSet<i64> = params.seeds.iter().copied().collect();
    let mut depth_of: HashMap<i64, i32> = HashMap::new();
    for &s in &params.seeds {
        depth_of.insert(s, 0);
    }
    // Nodes visited/traversed-through but excluded from the rendered node
    // list (namespace-like pass-through under a node-kind filter).
    let mut suppressed_from_render: HashSet<i64> = HashSet::new();

    let mut emitted_edge_ids: HashSet<i64> = HashSet::new();
    let mut frontier: Vec<i64> = visited.iter().copied().collect();

    let edge_sql = if params.forward {
        "SELECT id,type,source_node_id,target_node_id FROM edge WHERE source_node_id IN ({IN}) AND (type & ?) != 0"
    } else {
        "SELECT id,type,source_node_id,target_node_id FROM edge WHERE target_node_id IN ({IN}) AND (type & ?) != 0"
    };

    let mut level = 0;
    while !frontier.is_empty() && (params.depth == 0 || level < params.depth) {
        let mut rows: Vec<EdgeRow> = Vec::new();
        db.run_chunked_in(
            edge_sql,
            &frontier,
            &[Value::Integer(params.edge_kind_mask as i64)],
            |row| {
                rows.push(EdgeRow {
                    id: row.get::<_, i64>(0)?,
                    kind: row.get::<_, i32>(1)?,
                    source: row.get::<_, i64>(2)?,
                    target: row.get::<_, i64>(3)?,
                });
                Ok(())
            },
            CHUNK_SIZE,
        )?;

        let mut next_frontier: HashSet<i64> = HashSet::new();
        for row in &rows {
            if !emitted_edge_ids.contains(&row.id) {
                emitted_edge_ids.insert(row.id);
                result.edges.push(BfsEdge {
                    id: row.id,
                    kind: row.kind,
                    source: row.source,
                    target: row.target,
                });
            }

            let new_node = if params.forward { row.target } else { row.source };
            if !visited.contains(&new_node) {
                next_frontier.insert(new_node);
            }
        }

        if params.node_kind_mask != 0 && !next_frontier.is_empty() {
            let candidates: Vec<i64> = next_frontier.iter().copied().collect();
            let mut kind_of: HashMap<i64, i32> = HashMap::new();
            db.run_chunked_in(
                "SELECT id,type FROM node WHERE id IN ({IN})",
                &candidates,
                &[],
                |row| {
                    kind_of.insert(row.get::<_, i64>(0)?, row.get::<_, i32>(1)?);
                    Ok(())
                },
                CHUNK_SIZE,
            )?;

            let mut filtered: HashSet<i64> = HashSet::new();
            for &id in &candidates {
                let kind = kind_of.get(&id).copied().unwrap_or(0);
                if (kind & params.node_kind_mask) != 0 {
                    filtered.insert(id);
                } else if is_namespace_like_node_kind(kind) {
                    filtered.insert(id);
                    suppressed_from_render.insert(id);
                }
                // else: dropped entirely -- doesn't advance the frontier.
            }
            next_frontier = filtered;
        }

        if params.max_nodes != 0 {
            let allowed = if visited.len() >= params.max_nodes as usize {
                0
            } else {
                params.max_nodes as usize - visited.len()
            };
            if next_frontier.len() > allowed {
                let mut sorted_candidates: Vec<i64> = next_frontier.iter().copied().collect();
                sorted_candidates.sort_unstable();
                sorted_candidates.truncate(allowed);
                next_frontier = sorted_candidates.iter().copied().collect();
                result.truncated = true;

                for &id in &next_frontier {
                    depth_of.insert(id, level + 1);
                }
                visited.extend(next_frontier.iter().copied());
                // The C++ does `level += 1` here before `break`, but `level`
                // is never read after the loop, so the increment is dead.
                break;
            }
        }

        for &id in &next_frontier {
            depth_of.insert(id, level + 1);
        }
        visited.extend(next_frontier.iter().copied());
        frontier = next_frontier.into_iter().collect();
        level += 1;
    }

    let mut to_hydrate: Vec<i64> = Vec::new();
    for &id in &visited {
        if !suppressed_from_render.contains(&id) {
            to_hydrate.push(id);
        }
    }

    db.run_chunked_in(
        "SELECT id,type,serialized_name FROM node WHERE id IN ({IN})",
        &to_hydrate,
        &[],
        |row| {
            let id = row.get::<_, i64>(0)?;
            result.nodes.push(BfsNode {
                id,
                kind: row.get::<_, i32>(1)?,
                serialized_name: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                depth: depth_of.get(&id).copied().unwrap_or(0),
            });
            Ok(())
        },
        CHUNK_SIZE,
    )?;

    sort_nodes(&mut result.nodes);
    sort_edges(&mut result.edges);
    Ok(result)
}