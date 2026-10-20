//! Text, JSON, and DOT rendering for the `symhow graph` verbs.
//!
//! Additive sibling of `text_writer`/`json_writer` for the Rust-only `symhow
//! graph` subcommand (there is no C++ TextWriter/JsonWriter counterpart to
//! mirror). JSON follows docs/JSON_SCHEMA.md's envelope philosophy:
//! `symhow_version` + `schema` first, a `query` echo of what was asked, a
//! small `stats` object, `truncated` where a cap applies, alphabetical key
//! order (serde_json's BTreeMap default — the `preserve_order` feature must
//! stay off, same as `json_writer`), and arrays that preserve the producing
//! algorithm's order. New schemas:
//!
//! - `symhow.graph.centrality/1`
//! - `symhow.graph.pagerank/1`
//! - `symhow.graph.dominators/1`
//! - `symhow.graph.slice/1`
//! - `symhow.graph.path/1`
//!
//! Node objects reuse the canonical JSON_SCHEMA.md node-object shape (`id`,
//! `kind`, `name`, `qualified_name`, `signature`, `display_name` only when
//! the TU-local disambiguator is present) built from the graph layer's
//! already-decoded per-participant metadata. They deliberately OMIT the
//! DB-hydrated `indexed`/`definition_kind`/`location` fields: the graph
//! layer carries only {node id, kind, serialized_name} per participant, and
//! re-querying the database for every ranked node would turn the ranked
//! verbs into O(result) extra queries. Consumers join on `id` with
//! `symhow.graph/1` / `symhow.definition/1` output instead (flagged for the
//! JSON_SCHEMA.md update).
//!
//! Text output is compact and ranked: `score  #id  qualified-name` lines for
//! the scored verbs, an indented tree for dominators, a node list plus an
//! edge list (optionally annotated with per-edge source-line sites) for
//! slices, and the walked chain with per-hop weights for paths. Like
//! `text_writer`, these are pure functions returning the bytes to print.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use petgraph::visit::EdgeRef;
use serde_json::{json, Map, Value};

use crate::db::enums::{edge_kind_to_string, node_kind_to_string};
use crate::db::name_decode::decode_serialized_name;
use crate::graph::{
    BackwardSlice, CallGraph, DominatorTree, EdgeSite, GraphNodeData, PathWeight, ScoredNode,
    WeightedPath,
};

// --- shared building blocks ---------------------------------------------------

/// The canonical node object (docs/JSON_SCHEMA.md "The node object") for one
/// graph participant, built from the graph layer's decoded metadata. Omits
/// the DB-hydrated `indexed`/`definition_kind`/`location` fields (module doc).
fn node_json(node: &GraphNodeData) -> Value {
    let decoded = decode_serialized_name(&node.serialized_name);
    let mut j = Map::new();
    j.insert("id".to_string(), json!(node.node_id));
    j.insert("kind".to_string(), json!(node_kind_to_string(node.kind)));
    j.insert(
        "name".to_string(),
        json!(decoded
            .elements
            .last()
            .map(|element| element.name.as_str())
            .unwrap_or("")),
    );
    j.insert("qualified_name".to_string(), json!(decoded.name_path()));
    j.insert("signature".to_string(), json!(decoded.signature()));

    let qualified = decoded.qualified_name();
    let display = decoded.display_name();
    if display != qualified {
        // Only when the hidden TU-local disambiguator suffix was actually
        // present -- same rule as json_writer's node_object().
        j.insert("display_name".to_string(), json!(display));
    }
    Value::Object(j)
}

/// [`node_json`] for an arbitrary DB node id; defensive minimal fallback for
/// an id that is somehow not a participant of the loaded graph.
fn node_json_by_id(graph: &CallGraph, id: i64) -> Value {
    match graph.nodes.get(&id) {
        Some(node) => node_json(node),
        None => json!({
            "id": id,
            "kind": "",
            "name": "",
            "qualified_name": "",
            "signature": "",
        }),
    }
}

/// The name the text writers print: the decoded qualified name (name path +
/// signature postfix, TU-local disambiguator kept), so same-named TU-local
/// symbols stay distinguishable next to their `#id`.
fn qualified_name_of(graph: &CallGraph, id: i64) -> String {
    graph
        .nodes
        .get(&id)
        .map(|node| node.qualified_name.clone())
        .unwrap_or_default()
}

/// "file:start" / "file:start-end" for one edge reference site.
fn format_site(site: &EdgeSite) -> String {
    if site.end_line > site.start_line {
        format!("{}:{}-{}", site.file_path, site.start_line, site.end_line)
    } else {
        format!("{}:{}", site.file_path, site.start_line)
    }
}

/// The deduped (loader-ordered) "file:line" strings of one edge's sites.
fn site_strings(sites: &[EdgeSite]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(sites.len());
    for site in sites {
        let text = format_site(site);
        if seen.insert(text.clone()) {
            out.push(text);
        }
    }
    out
}

/// The deduped (loader-ordered) structured site objects for JSON.
fn site_jsons(sites: &[EdgeSite]) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(sites.len());
    for site in sites {
        let key = (site.file_path.as_str(), site.start_line, site.end_line);
        if seen.insert(key) {
            out.push(json!({
                "file": site.file_path,
                "start_line": site.start_line,
                "end_line": site.end_line,
            }));
        }
    }
    out
}

/// "CALL" / "USAGE x3" — the edge label shared by the text and DOT writers.
fn edge_label(kind: i32, call_sites: u32) -> String {
    if call_sites > 1 {
        format!("{} x{}", edge_kind_to_string(kind), call_sites)
    } else {
        edge_kind_to_string(kind)
    }
}

/// The graph-level facts the text writers open with: participant/edge counts,
/// the loaded edge kinds, and the unsolved-hub note (only when a hub node was
/// actually excluded).
fn graph_header(graph: &CallGraph) -> String {
    let mut out = format!(
        "{} nodes, {} edges ({})",
        graph.node_count(),
        graph.edge_count(),
        if graph.options.include_usage {
            "CALL+USAGE"
        } else {
            "CALL"
        }
    );
    if !graph.excluded_unsolved_ids.is_empty() {
        out.push_str("; unsolved hub excluded");
    }
    out
}

/// edge_id -> (kind, call-site count, target node id), for per-hop rendering
/// of a [`WeightedPath`] (which carries only edge ids).
fn edge_data_by_id(graph: &CallGraph) -> HashMap<i64, (i32, u32, i64)> {
    let mut out = HashMap::with_capacity(graph.edge_count());
    for reference in graph.graph.edge_references() {
        out.insert(
            reference.weight().edge_id,
            (
                reference.weight().kind,
                reference.weight().call_sites,
                graph.graph[reference.target()],
            ),
        );
    }
    out
}

/// The per-edge cost under `scheme` — mirrors `graph::path`'s cost closure
/// (hops: uniform 1; calls: the edge's site count; lines: the callee's body
/// line count).
fn edge_weight(graph: &CallGraph, scheme: PathWeight, call_sites: u32, target_id: i64) -> f64 {
    match scheme {
        PathWeight::Hops => 1.0,
        PathWeight::Calls => call_sites.max(1) as f64,
        PathWeight::Lines => graph
            .nodes
            .get(&target_id)
            .and_then(|node| node.body_lines)
            .unwrap_or(1)
            .max(1) as f64,
    }
}

/// `--weight` scheme name as printed/serialized.
pub fn path_weight_to_string(weight: PathWeight) -> &'static str {
    match weight {
        PathWeight::Hops => "hops",
        PathWeight::Calls => "calls",
        PathWeight::Lines => "lines",
    }
}

// --- text writers ---------------------------------------------------------------

/// The `score  #id  qualified-name` rows shared by the two scored verbs.
fn write_scored_rows(out: &mut String, graph: &CallGraph, ranked: &[ScoredNode], shown: usize) {
    if ranked.is_empty() {
        out.push_str("(no nodes)\n");
        return;
    }
    for node in ranked.iter().take(shown) {
        let _ = writeln!(
            out,
            "{:>12.6}  #{:<6}  {}",
            node.score,
            node.node_id,
            qualified_name_of(graph, node.node_id)
        );
    }
}

/// `symhow graph centrality` text mode.
pub fn write_centrality(graph: &CallGraph, ranked: &[ScoredNode], shown: usize) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "betweenness centrality (raw Brandes dependency sum) -- {}",
        graph_header(graph)
    );
    write_scored_rows(&mut out, graph, ranked, shown);
    if shown < ranked.len() {
        let _ = writeln!(
            out,
            "(showing top {} of {} nodes; --top 0 shows all)",
            shown,
            ranked.len()
        );
    }
    out
}

/// `symhow graph pagerank` text mode.
pub fn write_pagerank(
    graph: &CallGraph,
    ranked: &[ScoredNode],
    shown: usize,
    damping: f64,
    seed_count: usize,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "personalized PageRank (damping {}, {} seed(s)) -- {}",
        damping,
        seed_count,
        graph_header(graph)
    );
    write_scored_rows(&mut out, graph, ranked, shown);
    if shown < ranked.len() {
        let _ = writeln!(
            out,
            "(showing top {} of {} nodes; --top 0 shows all)",
            shown,
            ranked.len()
        );
    }
    out
}

/// `symhow graph dominators` text mode: the tree, indented two spaces per
/// dominator-tree level, children in ascending node-id order.
pub fn write_dominators(graph: &CallGraph, tree: &DominatorTree) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "dominator tree from {} (#{}) -- {} of {} nodes dominated",
        qualified_name_of(graph, tree.root),
        tree.root,
        tree.idoms.len() + 1,
        graph.node_count()
    );

    // idoms is sorted by node id, so each children list comes out ascending.
    let mut children: HashMap<i64, Vec<i64>> = HashMap::new();
    for &(node, idom) in &tree.idoms {
        children.entry(idom).or_default().push(node);
    }

    // Explicit stack (no recursion): pop order = pre-order DFS with siblings
    // visited in ascending id order.
    let mut stack: Vec<(i64, usize)> = vec![(tree.root, 0)];
    while let Some((id, depth)) = stack.pop() {
        let _ = writeln!(
            out,
            "{}#{:<6}  {}",
            "  ".repeat(depth),
            id,
            qualified_name_of(graph, id)
        );
        if let Some(kids) = children.get(&id) {
            for &kid in kids.iter().rev() {
                stack.push((kid, depth + 1));
            }
        }
    }
    out
}

/// `symhow graph slice` text mode: the member set, then the induced
/// dependence edges; with `show_lines`, each edge's deduped source-line
/// sites appended as `@ file:line, file:line`.
pub fn write_slice(graph: &CallGraph, slice: &BackwardSlice, show_lines: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "backward slice of {} (#{}) -- {} nodes, {} edges",
        qualified_name_of(graph, slice.criterion),
        slice.criterion,
        slice.nodes.len(),
        slice.edges.len()
    );
    for &id in &slice.nodes {
        let _ = writeln!(out, "#{:<6}  {}", id, qualified_name_of(graph, id));
    }
    if !slice.edges.is_empty() {
        out.push_str("edges:\n");
    }
    for edge in &slice.edges {
        let _ = write!(
            out,
            "  #{:<6} --{}--> #{:<6}",
            edge.source,
            edge_label(edge.kind, edge.call_sites),
            edge.target
        );
        if show_lines {
            let sites = site_strings(&edge.sites);
            if !sites.is_empty() {
                let _ = write!(out, "  @ {}", sites.join(", "));
            }
        }
        out.push('\n');
    }
    out
}

/// `symhow graph path` text mode: the walked chain, one node per line with
/// the per-hop edge (kind + weight) between them. `None` (target unreachable)
/// is the answer, not an error: a plain "no path" line, exit 0.
pub fn write_path(
    graph: &CallGraph,
    from_id: i64,
    to_id: i64,
    path: Option<&WeightedPath>,
) -> String {
    let Some(path) = path else {
        return format!(
            "no path from {} (#{}) to {} (#{})\n",
            qualified_name_of(graph, from_id),
            from_id,
            qualified_name_of(graph, to_id),
            to_id
        );
    };

    let mut out = String::new();
    let _ = writeln!(
        out,
        "shortest path {} (#{}) -> {} (#{}) [weight: {}] -- total cost {}, {} node(s)",
        qualified_name_of(graph, path.source),
        path.source,
        qualified_name_of(graph, path.target),
        path.target,
        path_weight_to_string(path.scheme),
        path.total_cost,
        path.nodes.len()
    );
    let edges = edge_data_by_id(graph);
    for (i, &id) in path.nodes.iter().enumerate() {
        if i > 0 {
            let (kind, call_sites, target_id) = path
                .edge_ids
                .get(i - 1)
                .and_then(|edge_id| edges.get(edge_id).copied())
                .unwrap_or((0, 0, id));
            let weight = edge_weight(graph, path.scheme, call_sites, target_id);
            let _ = write!(out, "  --{} w={}--> ", edge_label(kind, call_sites), weight);
        }
        let _ = writeln!(out, "#{:<6}  {}", id, qualified_name_of(graph, id));
    }
    out
}

// --- DOT emitters ---------------------------------------------------------------

/// Escapes a string for a DOT quoted attribute value (same rules as
/// `graph::render`'s private helper).
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

/// One DOT node line: the DB node id as the DOT node id (stable, unique),
/// `"display name (KIND)"` as the label — the same label convention
/// `graph::render`'s emitters use.
fn dot_node_line(graph: &CallGraph, id: i64) -> String {
    let (kind, name) = match graph.nodes.get(&id) {
        Some(node) => (node.kind, node.display_name.as_str()),
        None => (0, ""),
    };
    format!(
        "    {} [label=\"{}\"];\n",
        id,
        dot_escape(&format!("{} ({})", name, node_kind_to_string(kind)))
    )
}

/// DOT for a dominator tree: `idom -> node` tree edges (the root has none).
pub fn dominator_tree_dot(graph: &CallGraph, tree: &DominatorTree) -> String {
    let mut out = String::from("digraph dominators {\n");
    out.push_str(&dot_node_line(graph, tree.root));
    for &(node, _) in &tree.idoms {
        out.push_str(&dot_node_line(graph, node));
    }
    for &(node, idom) in &tree.idoms {
        let _ = writeln!(out, "    {} -> {};", idom, node);
    }
    out.push_str("}\n");
    out
}

/// DOT for a backward slice: the member set and its induced dependence edges;
/// with `show_lines`, edge labels carry the deduped source-line sites.
pub fn slice_dot(graph: &CallGraph, slice: &BackwardSlice, show_lines: bool) -> String {
    let mut out = String::from("digraph slice {\n");
    for &id in &slice.nodes {
        out.push_str(&dot_node_line(graph, id));
    }
    for edge in &slice.edges {
        let mut label = edge_label(edge.kind, edge.call_sites);
        if show_lines {
            let sites = site_strings(&edge.sites);
            if !sites.is_empty() {
                let _ = write!(label, " @ {}", sites.join(", "));
            }
        }
        let _ = writeln!(
            out,
            "    {} -> {} [label=\"{}\"];",
            edge.source,
            edge.target,
            dot_escape(&label)
        );
    }
    out.push_str("}\n");
    out
}

/// DOT for a shortest path: the walked chain with per-edge `KIND w=<cost>`
/// labels. An unreachable path renders as an empty (comment-only) digraph.
pub fn path_dot(graph: &CallGraph, path: Option<&WeightedPath>) -> String {
    let Some(path) = path else {
        return "digraph path {\n}\n".to_string();
    };
    let mut out = String::from("digraph path {\n");
    for &id in &path.nodes {
        out.push_str(&dot_node_line(graph, id));
    }
    let edges = edge_data_by_id(graph);
    for (i, edge_id) in path.edge_ids.iter().enumerate() {
        let (Some(&source), Some(&target)) = (path.nodes.get(i), path.nodes.get(i + 1)) else {
            continue;
        };
        let (kind, call_sites, _) = edges.get(edge_id).copied().unwrap_or((0, 0, 0));
        let _ = writeln!(
            out,
            "    {} -> {} [label=\"{}\"];",
            source,
            target,
            dot_escape(&edge_label(kind, call_sites))
        );
    }
    out.push_str("}\n");
    out
}

// --- JSON builders ---------------------------------------------------------------

/// The two fields every envelope starts with (docs/JSON_SCHEMA.md).
fn envelope(symhow_version: &str, schema: &str) -> Map<String, Value> {
    let mut j = Map::new();
    j.insert("symhow_version".to_string(), json!(symhow_version));
    j.insert("schema".to_string(), json!(schema));
    j
}

/// The ranked `nodes[]` shared by the two scored verbs: canonical node
/// objects plus their `score`, in ranked order.
fn scored_nodes_json(graph: &CallGraph, ranked: &[ScoredNode], shown: usize) -> Vec<Value> {
    ranked
        .iter()
        .take(shown)
        .map(|scored| {
            let mut o = node_json_by_id(graph, scored.node_id);
            o["score"] = json!(scored.score);
            o
        })
        .collect()
}

/// `symhow.graph.centrality/1`. `top` is the effective `--top` value (0 =
/// all); `shown` the number of rows actually in `nodes[]`; `truncated` is
/// true only when `--top` cut the full ranked list.
pub fn centrality_json(
    symhow_version: &str,
    db: &str,
    top: usize,
    include_unsolved: bool,
    graph: &CallGraph,
    ranked: &[ScoredNode],
    shown: usize,
    elapsed_ms: i64,
) -> Value {
    let mut j = envelope(symhow_version, "symhow.graph.centrality/1");

    let mut q = Map::new();
    q.insert("verb".to_string(), json!("centrality"));
    q.insert("db".to_string(), json!(db));
    q.insert("top".to_string(), json!(top));
    q.insert("include_unsolved".to_string(), json!(include_unsolved));
    j.insert("query".to_string(), Value::Object(q));

    j.insert("truncated".to_string(), json!(shown < ranked.len()));

    let node_count = graph.node_count();
    let mut stats = Map::new();
    stats.insert("node_count".to_string(), json!(node_count));
    stats.insert("edge_count".to_string(), json!(graph.edge_count()));
    stats.insert("shown".to_string(), json!(shown));
    stats.insert(
        "normalized_denominator".to_string(),
        if node_count > 2 {
            json!(((node_count - 1) * (node_count - 2)) as i64)
        } else {
            Value::Null
        },
    );
    stats.insert("elapsed_ms".to_string(), json!(elapsed_ms));
    j.insert("stats".to_string(), Value::Object(stats));

    j.insert(
        "nodes".to_string(),
        Value::Array(scored_nodes_json(graph, ranked, shown)),
    );
    Value::Object(j)
}

/// `symhow.graph.pagerank/1`. `seed_specs` echoes the raw `--seed` values
/// (empty when the entry-point fallback ran); `seed_ids` the ids actually
/// used (explicit or fallback); `seeds_explicit` distinguishes the two.
pub fn pagerank_json(
    symhow_version: &str,
    db: &str,
    seed_specs: &[String],
    seed_ids: &[i64],
    seeds_explicit: bool,
    damping: f64,
    top: usize,
    graph: &CallGraph,
    ranked: &[ScoredNode],
    shown: usize,
    elapsed_ms: i64,
) -> Value {
    let mut j = envelope(symhow_version, "symhow.graph.pagerank/1");

    let mut q = Map::new();
    q.insert("verb".to_string(), json!("pagerank"));
    q.insert("db".to_string(), json!(db));
    q.insert("seeds".to_string(), json!(seed_specs));
    q.insert("seed_ids".to_string(), json!(seed_ids));
    q.insert("seeds_explicit".to_string(), json!(seeds_explicit));
    q.insert("damping".to_string(), json!(damping));
    q.insert("top".to_string(), json!(top));
    j.insert("query".to_string(), Value::Object(q));

    j.insert("truncated".to_string(), json!(shown < ranked.len()));

    let mut stats = Map::new();
    stats.insert("node_count".to_string(), json!(graph.node_count()));
    stats.insert("edge_count".to_string(), json!(graph.edge_count()));
    stats.insert("shown".to_string(), json!(shown));
    stats.insert("seed_count".to_string(), json!(seed_ids.len()));
    stats.insert("elapsed_ms".to_string(), json!(elapsed_ms));
    j.insert("stats".to_string(), Value::Object(stats));

    j.insert(
        "nodes".to_string(),
        Value::Array(scored_nodes_json(graph, ranked, shown)),
    );
    Value::Object(j)
}

/// `symhow.graph.dominators/1`: the root, every dominated node as a canonical
/// node object (sorted by id), and the `idom` pairs as id pairs (sorted by
/// node id, the API's own order). `entry_spec` is the raw `--entry` value, or
/// "" when the entry-point fallback chose the root.
pub fn dominators_json(
    symhow_version: &str,
    db: &str,
    entry_spec: &str,
    entry_id: i64,
    entry_explicit: bool,
    graph: &CallGraph,
    tree: &DominatorTree,
    elapsed_ms: i64,
) -> Value {
    let mut j = envelope(symhow_version, "symhow.graph.dominators/1");

    let mut q = Map::new();
    q.insert("verb".to_string(), json!("dominators"));
    q.insert("db".to_string(), json!(db));
    q.insert("entry".to_string(), json!(entry_spec));
    q.insert("entry_id".to_string(), json!(entry_id));
    q.insert("entry_explicit".to_string(), json!(entry_explicit));
    j.insert("query".to_string(), Value::Object(q));

    // No cap concept: the tree is reported whole.
    j.insert("truncated".to_string(), json!(false));

    let mut stats = Map::new();
    stats.insert("node_count".to_string(), json!(graph.node_count()));
    stats.insert("edge_count".to_string(), json!(graph.edge_count()));
    stats.insert("dominated_count".to_string(), json!(tree.idoms.len()));
    stats.insert("elapsed_ms".to_string(), json!(elapsed_ms));
    j.insert("stats".to_string(), Value::Object(stats));

    j.insert("root_id".to_string(), json!(tree.root));

    // nodes[]: the root plus every dominated node, sorted by id.
    let mut ids = std::collections::BTreeSet::new();
    ids.insert(tree.root);
    for &(node, _) in &tree.idoms {
        ids.insert(node);
    }
    let nodes: Vec<Value> = ids.iter().map(|&id| node_json_by_id(graph, id)).collect();
    j.insert("nodes".to_string(), Value::Array(nodes));

    let idoms: Vec<Value> = tree
        .idoms
        .iter()
        .map(|&(node, idom)| json!({ "node": node, "idom": idom }))
        .collect();
    j.insert("idoms".to_string(), Value::Array(idoms));

    Value::Object(j)
}

/// `symhow.graph.slice/1`: the member set (sorted ascending by id, the API's
/// own order) and the induced dependence edges with their reference-site
/// locations. The `--lines` flag is a text-mode presentation flag; the JSON
/// always carries the sites (they are the slice's core data).
pub fn slice_json(
    symhow_version: &str,
    db: &str,
    criterion_spec: &str,
    criterion_id: i64,
    show_lines: bool,
    graph: &CallGraph,
    slice: &BackwardSlice,
    elapsed_ms: i64,
) -> Value {
    let mut j = envelope(symhow_version, "symhow.graph.slice/1");

    let mut q = Map::new();
    q.insert("verb".to_string(), json!("slice"));
    q.insert("db".to_string(), json!(db));
    q.insert("criterion".to_string(), json!(criterion_spec));
    q.insert("criterion_id".to_string(), json!(criterion_id));
    q.insert("lines".to_string(), json!(show_lines));
    j.insert("query".to_string(), Value::Object(q));

    // No cap concept: the slice is reported whole.
    j.insert("truncated".to_string(), json!(false));

    let mut stats = Map::new();
    stats.insert("member_count".to_string(), json!(slice.nodes.len()));
    stats.insert("edge_count".to_string(), json!(slice.edges.len()));
    stats.insert("elapsed_ms".to_string(), json!(elapsed_ms));
    j.insert("stats".to_string(), Value::Object(stats));

    let nodes: Vec<Value> = slice
        .nodes
        .iter()
        .map(|&id| node_json_by_id(graph, id))
        .collect();
    j.insert("nodes".to_string(), Value::Array(nodes));

    let edges: Vec<Value> = slice
        .edges
        .iter()
        .map(|edge| {
            json!({
                "id": edge.edge_id,
                "kind": edge_kind_to_string(edge.kind),
                "source": edge.source,
                "target": edge.target,
                "call_sites": edge.call_sites,
                "locations": site_jsons(&edge.sites),
            })
        })
        .collect();
    j.insert("edges".to_string(), Value::Array(edges));

    Value::Object(j)
}

/// `symhow.graph.path/1`. `found` is false (with empty `nodes`/`edges` and a
/// null `stats.total_cost`) when the target is unreachable — that is a
/// result, not an error (exit 0 in both modes).
pub fn path_json(
    symhow_version: &str,
    db: &str,
    from_spec: &str,
    from_id: i64,
    to_spec: &str,
    to_id: i64,
    weight: PathWeight,
    graph: &CallGraph,
    path: Option<&WeightedPath>,
    elapsed_ms: i64,
) -> Value {
    let mut j = envelope(symhow_version, "symhow.graph.path/1");

    let mut q = Map::new();
    q.insert("verb".to_string(), json!("path"));
    q.insert("db".to_string(), json!(db));
    q.insert("from".to_string(), json!(from_spec));
    q.insert("from_id".to_string(), json!(from_id));
    q.insert("to".to_string(), json!(to_spec));
    q.insert("to_id".to_string(), json!(to_id));
    q.insert("weight".to_string(), json!(path_weight_to_string(weight)));
    j.insert("query".to_string(), Value::Object(q));

    // No cap concept: a path is reported whole (or not at all).
    j.insert("truncated".to_string(), json!(false));

    let mut stats = Map::new();
    if let Some(path) = path {
        stats.insert("total_cost".to_string(), json!(path.total_cost));
        stats.insert("hop_count".to_string(), json!(path.nodes.len() - 1));
        stats.insert("node_count".to_string(), json!(path.nodes.len()));
    } else {
        stats.insert("total_cost".to_string(), Value::Null);
        stats.insert("hop_count".to_string(), json!(0));
        stats.insert("node_count".to_string(), json!(0));
    }
    stats.insert("elapsed_ms".to_string(), json!(elapsed_ms));
    j.insert("stats".to_string(), Value::Object(stats));

    j.insert("found".to_string(), json!(path.is_some()));

    let edges_map = edge_data_by_id(graph);
    let mut nodes_json: Vec<Value> = Vec::new();
    let mut edges_json: Vec<Value> = Vec::new();
    if let Some(path) = path {
        for (i, &id) in path.nodes.iter().enumerate() {
            nodes_json.push(node_json_by_id(graph, id));
            if i > 0 {
                let (kind, call_sites, target_id) = path
                    .edge_ids
                    .get(i - 1)
                    .and_then(|edge_id| edges_map.get(edge_id).copied())
                    .unwrap_or((0, 0, id));
                edges_json.push(json!({
                    "id": path.edge_ids[i - 1],
                    "kind": edge_kind_to_string(kind),
                    "source": path.nodes[i - 1],
                    "target": id,
                    "weight": edge_weight(graph, path.scheme, call_sites, target_id),
                }));
            }
        }
    }
    j.insert("nodes".to_string(), Value::Array(nodes_json));
    j.insert("edges".to_string(), Value::Array(edges_json));

    Value::Object(j)
}
