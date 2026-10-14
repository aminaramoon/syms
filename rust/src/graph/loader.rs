//! Graph loading: builds the call/usage graph a `.srctrldb` database
//! describes, and resolves entry-point seeds for the graph algorithms.
//!
//! Everything here is read-only and additive over `src/db/`: the SQL follows
//! `docs/GRAPH_SCHEMA_NOTES.md` §4 (verified against real fixture databases)
//! and all bulk lookups go through `Database::run_chunked_in`. Loading is
//! isolated in this one module so the storage can later swap to a CSR
//! representation without touching the algorithm modules.
//!
//! Schema facts this module relies on (see GRAPH_SCHEMA_NOTES.md):
//! - `node.type` is a single-bit NodeKind mask; `(type & mask) != 0` filters.
//! - `edge` is unique on (source, target, type) — no parallel edges; the
//!   call-site multiplicity of a CALL edge is its `occurrence` row count
//!   (§4.2).
//! - Every non-MEMBER edge has at least one `occurrence` row, whose
//!   `source_location` is the source-code site of the reference; that is
//!   where per-edge source lines come from (§2.4, §4.4).
//! - `node.type = 1` (UNKNOWN) is the single shared "unsolved symbol" hub
//!   node that collects every unresolved reference; excluded by default
//!   because it would dominate centrality/PageRank results.
//! - `meta` must never be `SELECT *`-ed (`symhow_dirty_paths` is ~2 MB);
//!   `Database::open()` already reads the only key this module needs.

use std::collections::{BTreeSet, HashMap, HashSet};

use petgraph::graph::{DiGraph, NodeIndex};

use crate::common::Result;
use crate::db::enums::{edge_kind, node_kind, node_kind_to_string};
use crate::db::name_decode::decode_serialized_name;
use crate::db::Database;
use crate::query::resolve_symbol;

/// Chunk size for the chunked-IN helpers (the C++ `runChunkedIn` default,
/// same value as the query layer's).
const CHUNK_SIZE: usize = 500;

/// FUNCTION | METHOD — the node kinds that participate in the call graph.
const CALLABLE_KIND_MASK: i32 = node_kind::FUNCTION | node_kind::METHOD;

/// Options controlling what [`load_call_graph`] loads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphLoadOptions {
    /// Also load USAGE edges (`edge.type = 4`: field/global/enum-constant/
    /// function-reference accesses) alongside CALL edges. Required by
    /// [`crate::graph::backward_slice`]; with it on, the accessed symbols
    /// (fields, globals, ...) become graph participants too, and every
    /// algorithm sees the combined edge set.
    pub include_usage: bool,
    /// Load per-function body-line sizes (GRAPH_SCHEMA_NOTES §4.6, each
    /// function's SCOPE span). Required by
    /// [`crate::graph::PathWeight::Lines`].
    pub with_body_lines: bool,
    /// Exclude the single shared "unsolved symbol" hub node
    /// (`node.type = 1`) and every edge touching it.
    pub exclude_unsolved: bool,
}

impl Default for GraphLoadOptions {
    fn default() -> Self {
        GraphLoadOptions {
            include_usage: true,
            with_body_lines: false,
            exclude_unsolved: true,
        }
    }
}

/// One source-code site of an edge reference (the line of the call/access),
/// from the edge's `occurrence` rows joined to `source_location` (§4.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeSite {
    /// `source_location.file_node_id` — the file the site is in (the same id
    /// as the file's `node` row).
    pub file_node_id: i64,
    /// `file.path` for that file (empty when the file row is missing).
    pub file_path: String,
    pub start_line: i32,
    pub end_line: i32,
}

/// Metadata for one graph participant (a `node` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphNodeData {
    /// `node.id`.
    pub node_id: i64,
    /// `node.type` NodeKind bit mask.
    pub kind: i32,
    /// Raw `node.serialized_name`.
    pub serialized_name: String,
    /// Bare leaf element name — no ancestors, no signature (the `name`
    /// field of the existing JSON node objects, e.g. `main`).
    pub name: String,
    /// Decoded display name: name path plus the last element's signature
    /// postfix, TU-local disambiguator stripped (e.g. `main()`; both
    /// TU-local mains display identically).
    pub display_name: String,
    /// Decoded qualified name — like `display_name` but with the TU-local
    /// disambiguator kept (`main() [tu_a.cpp#04c3b899]`), which
    /// distinguishes same-named TU-local symbols.
    pub qualified_name: String,
    /// Body line count from the function's SCOPE span (§4.6); `None` when
    /// not loaded (`GraphLoadOptions::with_body_lines` off) or when the node
    /// has no SCOPE span.
    pub body_lines: Option<u32>,
}

/// Payload of one graph edge (a CALL or USAGE `edge` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEdgeData {
    /// `edge.id`.
    pub edge_id: i64,
    /// EdgeKind bit: `edge_kind::CALL` (8) or `edge_kind::USAGE` (4).
    pub kind: i32,
    /// Number of occurrence rows for this edge — call sites for CALL edges,
    /// reference sites for USAGE edges (§2.4/§4.2).
    pub call_sites: u32,
    /// The reference sites, sorted by (file path, start line).
    pub sites: Vec<EdgeSite>,
}

/// The loaded graph: a `petgraph::graph::DiGraph` with node weight = DB node
/// id, plus the id -> NodeIndex side map and per-node metadata.
///
/// Node participants are every FUNCTION/METHOD node plus every endpoint of a
/// loaded edge; edges are CALL (and, with `include_usage`, USAGE) edges with
/// the unsolved hub (and its edges) removed by default.
pub struct CallGraph {
    /// Node weight = DB node id (`i64`); edge weight = [`GraphEdgeData`].
    pub graph: DiGraph<i64, GraphEdgeData>,
    /// DB node id -> petgraph node index.
    pub index_of: HashMap<i64, NodeIndex>,
    /// Per-node metadata for every participant (keyed by DB node id).
    pub nodes: HashMap<i64, GraphNodeData>,
    /// The unsolved-hub node ids that were excluded (empty when
    /// `exclude_unsolved` is off or the DB has no UNKNOWN node).
    pub excluded_unsolved_ids: Vec<i64>,
    /// The options this graph was loaded with.
    pub options: GraphLoadOptions,
}

impl CallGraph {
    /// The petgraph node index for a DB node id, if it participates.
    pub fn index(&self, node_id: i64) -> Option<NodeIndex> {
        self.index_of.get(&node_id).copied()
    }

    /// The DB node id for a petgraph node index (the node weight).
    pub fn node_id(&self, index: NodeIndex) -> i64 {
        self.graph[index]
    }

    /// The metadata for a DB node id, if present.
    pub fn node_data(&self, node_id: i64) -> Option<&GraphNodeData> {
        self.nodes.get(&node_id)
    }

    /// Number of participants (graph nodes).
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Number of loaded edges.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }
}

/// Loads the call graph (CALL edges, §4.2) — and, per `options`, USAGE edges
/// with per-edge source-line sites (§4.4) and per-function body-line sizes
/// (§4.6) — from an open read-only [`Database`].
///
/// The unsolved hub node and every edge touching it are dropped when
/// `options.exclude_unsolved` is set. Node insertion (and therefore
/// petgraph `NodeIndex` assignment) happens in ascending DB-id order and
/// edges are added in (source, target, kind, id) order, so two loads of the
/// same database with the same options yield identical graphs.
pub fn load_call_graph(db: &Database, options: &GraphLoadOptions) -> Result<CallGraph> {
    // 1. The unsolved hub ids (§2.1: `node.type = 1` is the single shared
    //    hub node; more than one row would be a bug, but exclude them all).
    let mut unsolved_ids: Vec<i64> = Vec::new();
    if options.exclude_unsolved {
        db.run("SELECT id FROM node WHERE type = 1", [], |row| {
            unsolved_ids.push(row.get::<_, i64>(0)?);
            Ok(())
        })?;
    }
    unsolved_ids.sort_unstable();
    let unsolved: HashSet<i64> = unsolved_ids.iter().copied().collect();

    // 2. CALL (+ USAGE) edges. Plain edge scan first (so an edge with zero
    //    occurrence rows could never be dropped by an occurrence JOIN),
    //    then the per-edge sites via the chunked occurrence join (§4.4).
    struct RawEdge {
        edge_id: i64,
        kind: i32,
        source: i64,
        target: i64,
    }
    let edge_mask = if options.include_usage {
        edge_kind::CALL | edge_kind::USAGE
    } else {
        edge_kind::CALL
    };
    let mut raw_edges: Vec<RawEdge> = Vec::new();
    db.run(
        "SELECT id, type, source_node_id, target_node_id FROM edge WHERE (type & ?) != 0",
        [edge_mask as i64],
        |row| {
            raw_edges.push(RawEdge {
                edge_id: row.get::<_, i64>(0)?,
                kind: row.get::<_, i32>(1)?,
                source: row.get::<_, i64>(2)?,
                target: row.get::<_, i64>(3)?,
            });
            Ok(())
        },
    )?;
    raw_edges.retain(|e| !unsolved.contains(&e.source) && !unsolved.contains(&e.target));

    // 3. Per-edge reference sites (§4.4): one row per (edge, location).
    let mut edge_ids: Vec<i64> = raw_edges.iter().map(|e| e.edge_id).collect();
    edge_ids.sort_unstable();
    let mut sites_by_edge: HashMap<i64, Vec<EdgeSite>> = HashMap::new();
    db.run_chunked_in(
        "SELECT o.element_id, l.file_node_id, l.start_line, l.end_line \
         FROM occurrence o JOIN source_location l ON l.id = o.source_location_id \
         WHERE o.element_id IN ({IN})",
        &edge_ids,
        &[],
        |row| {
            sites_by_edge
                .entry(row.get::<_, i64>(0)?)
                .or_default()
                .push(EdgeSite {
                    file_node_id: row.get::<_, i64>(1)?,
                    file_path: String::new(),
                    start_line: row.get::<_, i32>(2)?,
                    end_line: row.get::<_, i32>(3)?,
                });
            Ok(())
        },
        CHUNK_SIZE,
    )?;

    // File paths for the sites (file.id == the file's node id).
    let site_file_ids: BTreeSet<i64> = sites_by_edge
        .values()
        .flat_map(|sites| sites.iter().map(|s| s.file_node_id))
        .collect();
    let site_file_ids: Vec<i64> = site_file_ids.into_iter().collect();
    let mut file_paths: HashMap<i64, String> = HashMap::new();
    db.run_chunked_in(
        "SELECT id, path FROM file WHERE id IN ({IN})",
        &site_file_ids,
        &[],
        |row| {
            file_paths.insert(
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            );
            Ok(())
        },
        CHUNK_SIZE,
    )?;
    for sites in sites_by_edge.values_mut() {
        for site in sites.iter_mut() {
            site.file_path = file_paths
                .get(&site.file_node_id)
                .cloned()
                .unwrap_or_default();
        }
        sites.sort_by(|a, b| {
            a.file_path
                .cmp(&b.file_path)
                .then(a.start_line.cmp(&b.start_line))
                .then(a.end_line.cmp(&b.end_line))
        });
    }

    // 4. Participants: every FUNCTION/METHOD node (§4.1) plus every endpoint
    //    of a loaded edge (USAGE targets are fields/globals/...). With the
    //    hub excluded, its edges are already gone, so it never enters here.
    let mut participants: BTreeSet<i64> = BTreeSet::new();
    db.run(
        "SELECT id FROM node WHERE (type & ?) != 0",
        [CALLABLE_KIND_MASK as i64],
        |row| {
            participants.insert(row.get::<_, i64>(0)?);
            Ok(())
        },
    )?;
    for edge in &raw_edges {
        participants.insert(edge.source);
        participants.insert(edge.target);
    }
    let participant_ids: Vec<i64> = participants.into_iter().collect();

    // 5. Hydrate the participant node rows (chunked, like bfs.rs).
    let mut nodes: HashMap<i64, GraphNodeData> = HashMap::with_capacity(participant_ids.len());
    db.run_chunked_in(
        "SELECT id, type, serialized_name FROM node WHERE id IN ({IN})",
        &participant_ids,
        &[],
        |row| {
            let id = row.get::<_, i64>(0)?;
            let kind = row.get::<_, i32>(1)?;
            let serialized = row.get::<_, Option<String>>(2)?.unwrap_or_default();
            let decoded = decode_serialized_name(&serialized);
            nodes.insert(
                id,
                GraphNodeData {
                    node_id: id,
                    kind,
                    name: decoded
                        .elements
                        .last()
                        .map(|element| element.name.clone())
                        .unwrap_or_default(),
                    serialized_name: serialized,
                    display_name: decoded.display_name(),
                    qualified_name: decoded.qualified_name(),
                    body_lines: None,
                },
            );
            Ok(())
        },
        CHUNK_SIZE,
    )?;

    // 6. Body-line sizes (§4.6): the SCOPE span of each defined
    //    function/method. MAX() defends against multiple SCOPE occurrences.
    if options.with_body_lines {
        let body_lines_sql = format!(
            "SELECT o.element_id, MAX(l.end_line - l.start_line + 1) \
             FROM occurrence o JOIN source_location l ON l.id = o.source_location_id \
             WHERE l.type = 1 AND o.element_id IN ({{IN}}) \
               AND o.element_id IN (SELECT id FROM node WHERE (type & {}) != 0) \
             GROUP BY o.element_id",
            CALLABLE_KIND_MASK
        );
        db.run_chunked_in(
            &body_lines_sql,
            &participant_ids,
            &[],
            |row| {
                if let Some(node) = nodes.get_mut(&row.get::<_, i64>(0)?) {
                    node.body_lines = Some(row.get::<_, i32>(1)?.max(0) as u32);
                }
                Ok(())
            },
            CHUNK_SIZE,
        )?;
    }

    // 7. Build the petgraph. Nodes in ascending id order, then edges sorted,
    //    for deterministic index assignment across runs.
    let mut graph = DiGraph::with_capacity(participant_ids.len(), raw_edges.len());
    let mut index_of: HashMap<i64, NodeIndex> = HashMap::with_capacity(participant_ids.len());
    for &id in &participant_ids {
        let index = graph.add_node(id);
        index_of.insert(id, index);
    }
    raw_edges.sort_by(|a, b| {
        (a.source, a.target, a.kind, a.edge_id).cmp(&(b.source, b.target, b.kind, b.edge_id))
    });
    for edge in &raw_edges {
        let (Some(source), Some(target)) = (index_of.get(&edge.source), index_of.get(&edge.target))
        else {
            continue; // unreachable: every endpoint is a participant
        };
        let sites = sites_by_edge.remove(&edge.edge_id).unwrap_or_default();
        graph.add_edge(
            *source,
            *target,
            GraphEdgeData {
                edge_id: edge.edge_id,
                kind: edge.kind,
                call_sites: sites.len() as u32,
                sites,
            },
        );
    }

    Ok(CallGraph {
        graph,
        index_of,
        nodes,
        excluded_unsolved_ids: unsolved_ids,
        options: *options,
    })
}

/// Resolves entry-point seed node ids for the seed-driven graph algorithms
/// (`personalized_pagerank`, `dominator_tree`).
///
/// Resolution order:
/// 1. `seeds` — resolved with the same matcher as the existing
///    `callers`/`callees` seeds (`query::resolve_symbol`: `#id`,
///    qualified/display/leaf name). Every candidate that participates in
///    `graph` is accepted, so an ambiguous name like `main` legitimately
///    contributes several entry points (TU-local mains are distinct nodes).
/// 2. Fallback when that yields nothing: FUNCTION nodes whose decoded leaf
///    name is exactly `main` (again possibly several).
/// 3. Fallback when that yields nothing too: functions/methods belonging to
///    "root files" — files with zero incoming INCLUDE edges — found via
///    `symhow_symbol_def` when that table exists, plus (always) MEMBER-edge
///    descendants of the root files.
///
/// Returns distinct DB node ids sorted ascending; empty only when no
/// strategy produced anything (callers decide whether that is an error).
/// CLI flag wiring stays outside this module: it takes plain `&[String]`.
pub fn resolve_entry_points(
    db: &Database,
    graph: &CallGraph,
    seeds: &[String],
) -> Result<Vec<i64>> {
    // 1. Explicit seed specs.
    if !seeds.is_empty() {
        let mut ids: BTreeSet<i64> = BTreeSet::new();
        for spec in seeds {
            let resolved = resolve_symbol(db, spec, None, None)?;
            for candidate in resolved.candidates {
                if graph.index_of.contains_key(&candidate.id) {
                    ids.insert(candidate.id);
                }
            }
        }
        if !ids.is_empty() {
            return Ok(ids.into_iter().collect());
        }
    }

    // 2. Decoded-name `main` heuristic (FUNCTION nodes only).
    let mut mains: BTreeSet<i64> = BTreeSet::new();
    db.run(
        "SELECT id, serialized_name FROM node WHERE (type & ?) != 0",
        [node_kind::FUNCTION as i64],
        |row| {
            let id = row.get::<_, i64>(0)?;
            let serialized = row.get::<_, Option<String>>(1)?.unwrap_or_default();
            let decoded = decode_serialized_name(&serialized);
            let leaf = decoded
                .elements
                .last()
                .map(|element| element.name.as_str())
                .unwrap_or("");
            if leaf == "main" && graph.index_of.contains_key(&id) {
                mains.insert(id);
            }
            Ok(())
        },
    )?;
    if !mains.is_empty() {
        return Ok(mains.into_iter().collect());
    }

    // 3. Root-file descendants.
    let mut root_files: Vec<i64> = Vec::new();
    let root_files_sql = format!(
        "SELECT id FROM file f \
         WHERE NOT EXISTS (SELECT 1 FROM edge e WHERE e.type = {} AND e.target_node_id = f.id)",
        edge_kind::INCLUDE
    );
    db.run(&root_files_sql, [], |row| {
        root_files.push(row.get::<_, i64>(0)?);
        Ok(())
    })?;

    let mut candidates: BTreeSet<i64> = BTreeSet::new();

    // 3a. Via the symhow side table (declared/defined-in file per symbol).
    let mut table_count: i64 = 0;
    db.run(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'symhow_symbol_def'",
        [],
        |row| {
            table_count = row.get::<_, i64>(0)?;
            Ok(())
        },
    )?;
    if table_count > 0 {
        db.run_chunked_in(
            "SELECT DISTINCT node_id FROM symhow_symbol_def WHERE file_node_id IN ({IN})",
            &root_files,
            &[],
            |row| {
                candidates.insert(row.get::<_, i64>(0)?);
                Ok(())
            },
            CHUNK_SIZE,
        )?;
    }

    // 3b. MEMBER-edge descendants of the root files (covers databases
    //     without symhow side tables, where symbols hang off containers).
    let member_sql = format!(
        "SELECT target_node_id FROM edge WHERE type = {} AND source_node_id IN ({{IN}})",
        edge_kind::MEMBER
    );
    let mut seen: HashSet<i64> = root_files.iter().copied().collect();
    let mut frontier: Vec<i64> = root_files.clone();
    while !frontier.is_empty() {
        let mut next: Vec<i64> = Vec::new();
        db.run_chunked_in(
            &member_sql,
            &frontier,
            &[],
            |row| {
                let id = row.get::<_, i64>(0)?;
                if seen.insert(id) {
                    next.push(id);
                }
                Ok(())
            },
            CHUNK_SIZE,
        )?;
        candidates.extend(next.iter().copied());
        frontier = next;
    }

    let entry_points: Vec<i64> = candidates
        .into_iter()
        .filter(|id| {
            graph
                .nodes
                .get(id)
                .is_some_and(|node| (node.kind & CALLABLE_KIND_MASK) != 0)
        })
        .collect();
    Ok(entry_points)
}

/// Human-readable kind name for a participant (used by DOT labels;
/// `UNKNOWN(n)` for out-of-table values, mirroring `db::enums`).
pub(crate) fn participant_kind_name(kind: i32) -> String {
    node_kind_to_string(kind)
}
