//! Machine-readable (JSON) rendering of `QueryEngine` result types -- the
//! sibling of `text_writer` (Phase 4). Mirrors `symhow::out::json::JsonWriter`
//! (`src/out/JsonWriter.cpp`). See docs/JSON_SCHEMA.md for the full envelope
//! spec this implements (schema names, worked examples, and the two
//! "invented, not literally from the DB schema" conventions: edge_kind
//! "DECLARATION" in symhow.references/1, and is_local_symbol in symhow.file/1).
//!
//! Key-order note: the C++ uses plain `nlohmann::json` (std::map-based), so
//! object keys serialize in **alphabetical order**, not insertion order.
//! This port relies on serde_json's default `Map` (a BTreeMap, i.e. the
//! same byte-lexicographic order) -- the `preserve_order` feature must stay
//! off for byte-for-byte parity.
//!
//! Design note: `text_writer` is a set of PURE functions over
//! already-computed result structs -- it needs nothing else. JSON output
//! needs a little more per node than those structs carry (a single best
//! "definition-ish" location, symbol.indexed/definition_kind, and -- for
//! the `file` verb's occurrences -- local_symbol table membership). Rather
//! than growing the result structs with fields only JSON output needs,
//! `JsonWriter` hydrates that extra data itself via a handful of small
//! batched queries against the same read-only `Database` its paired
//! `QueryEngine` is already using (see `hydrate_nodes()`). The ONE exception
//! is `SearchHit::matched_indices`, added directly to the query layer's
//! result struct -- the fuzzy-match character positions aren't recoverable
//! from a node id alone.

use std::collections::{HashMap, HashSet};
use std::fs;

use serde_json::{json, Map, Value};

use crate::common::Result;
use crate::db::enums::{
    definition_kind_to_string, edge_kind, edge_kind_to_string, location_kind,
    location_kind_to_string, node_kind, node_kind_to_string,
};
use crate::db::name_decode::decode_serialized_name;
use crate::db::{missing_index_names, Database};
use crate::query::{
    BfsResult, DefinitionResult, FileLocation, FileResult, FuzzyHit, QueryEngine,
    ReferencesResult, ResolvedSymbol, SearchHit, StatsResult,
};

/// Chunk size for the chunked-IN helpers (the C++ `runChunkedIn` default,
/// same value as the query layer's).
const CHUNK_SIZE: usize = 500;

/// Everything about a `symhow query` invocation that a JSON envelope's
/// "query" object needs, gathered in one place so each schema builder below
/// can pull out just the fields relevant to it. Not every field is
/// meaningful for every verb (e.g. `depth`/`max_nodes`/`edge_kinds` are
/// graph-verb-only) -- see the method bodies for exactly which fields each
/// schema builder reads. Mirrors `symhow::out::json::QueryContext`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryContext {
    pub verb: String,
    pub db: String,
    /// The raw positional argument (SYMBOL/TEXT/PATH), if any.
    pub symbol_or_text_or_path: String,
    /// Resolved BFS seed(s) / symbol id(s), if applicable.
    pub symbol_ids: Vec<i64>,

    // Graph verbs only (callers/callees/bases/derived/overrides/
    // overridden-by/members/neighbors).
    pub depth: i32,
    pub max_nodes: i32,
    /// `edge_kind` values actually in play for this call.
    pub edge_kinds: Vec<i32>,
    /// `node_kind` --kind filter, if any (empty = unfiltered).
    pub node_kinds: Vec<i32>,

    // `search` verb only.
    pub limit: i32,
    pub search_kinds: Vec<i32>,
}

/// Per-node data JSON needs beyond what BfsNode/SearchHit/etc. carry.
/// `definition_kind == 0` means "no symbol row" (same convention as the
/// schema's DefinitionKind, which has no NONE value upstream).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct HydratedNode {
    found: bool,
    kind: i32,
    serialized_name: String,
    indexed: bool,
    definition_kind: i32,
    location: Option<FileLocation>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SymbolMeta {
    indexed: bool,
    definition_kind: i32,
}

// De-duplicates while preserving first-seen order (used before batched IN
// queries -- purely a size optimization, doesn't affect correctness).
fn dedup_preserve_order(ids: &[i64]) -> Vec<i64> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(ids.len());
    for &id in ids {
        if seen.insert(id) {
            out.push(id);
        }
    }
    out
}

/// Edge-kind bit values with a recorded occurrence location for at least one
/// edge in a well-formed corpus (i.e. every named `edge_kind` bit except
/// UNKNOWN/ALL), used to decompose an arbitrary OR'd edge-kind mask (e.g.
/// `neighbors`' default ALL) into the individual kind-name array the
/// symhow.graph/1 "query.edge_kinds" field wants. Mirrors the C++
/// `decomposeEdgeKindMask`; keep in sync with the edge-kind table in
/// `db::enums` if a new edge kind is ever added.
pub fn decompose_edge_kind_mask(mask: i32) -> Vec<i32> {
    let all_bits = [
        edge_kind::MEMBER,
        edge_kind::TYPE_USAGE,
        edge_kind::USAGE,
        edge_kind::CALL,
        edge_kind::INHERITANCE,
        edge_kind::OVERRIDE,
        edge_kind::TYPE_ARGUMENT,
        edge_kind::TEMPLATE_SPECIALIZATION,
        edge_kind::INCLUDE,
        edge_kind::IMPORT,
        edge_kind::MACRO_USAGE,
        edge_kind::ANNOTATION_USAGE,
    ];
    let mut out = Vec::new();
    for bit in all_bits {
        if (bit & mask) != 0 {
            out.push(bit);
        }
    }
    out
}

/// Machine-readable (JSON) rendering of query results. `db`/`engine` are
/// borrowed for the writer's lifetime (same lifetime contract as the C++
/// holding `Database&`/`QueryEngine&` references). `symhow_version` is the
/// same string `symhow --version` prints (SYMHOW_VERSION_STRING) -- passed
/// in rather than read from a constant here so this module has zero
/// compile-time coupling to how the CLI target is built.
pub struct JsonWriter<'a> {
    db: &'a Database,
    engine: &'a QueryEngine<'a>,
    symhow_version: String,
}

impl<'a> JsonWriter<'a> {
    pub fn new(db: &'a Database, engine: &'a QueryEngine<'a>, symhow_version: impl Into<String>) -> Self {
        JsonWriter {
            db,
            engine,
            symhow_version: symhow_version.into(),
        }
    }

    // --- hydration helpers -------------------------------------------------

    /// Just the location half of `hydrate_nodes()`, factored out for callers
    /// (`ambiguous()`/`not_found()`) that build the rest of the node object
    /// from a `ResolvedSymbol` directly and only need the location lookup
    /// by id. Same SCOPE-preferred-over-TOKEN selection as
    /// `QueryEngine::definition()`, batched across every id at once instead
    /// of one query per id.
    fn load_best_locations(&self, ids_in: &[i64]) -> Result<HashMap<i64, FileLocation>> {
        let mut out = HashMap::new();
        let ids = dedup_preserve_order(ids_in);
        if ids.is_empty() {
            return Ok(out);
        }

        let mut scope_locs: HashMap<i64, Vec<FileLocation>> = HashMap::new();
        let mut token_locs: HashMap<i64, Vec<FileLocation>> = HashMap::new();

        self.db.run_chunked_in(
            "SELECT o.element_id, l.file_node_id, l.start_line, l.start_column, l.end_line, l.end_column, l.type \
             FROM occurrence o JOIN source_location l ON l.id = o.source_location_id \
             WHERE o.element_id IN ({IN}) AND l.type IN (0,1)",
            &ids,
            &[],
            |row| {
                let id = row.get::<_, i64>(0)?;
                let mut loc = FileLocation::default();
                loc.file_id = row.get::<_, i64>(1)?;
                loc.file_path = self
                    .engine
                    .file_map()
                    .get(&loc.file_id)
                    .cloned()
                    .unwrap_or_default();
                loc.start_line = row.get::<_, i32>(2)?;
                loc.start_column = row.get::<_, i32>(3)?;
                loc.end_line = row.get::<_, i32>(4)?;
                loc.end_column = row.get::<_, i32>(5)?;
                loc.location_kind = row.get::<_, i32>(6)?;
                if loc.location_kind == location_kind::SCOPE {
                    scope_locs.entry(id).or_default().push(loc);
                } else {
                    token_locs.entry(id).or_default().push(loc);
                }
                Ok(())
            },
            CHUNK_SIZE,
        )?;

        // Same (filePath, startLine, startColumn, fileId) tie-break the
        // query layer applies to its location lists.
        let sort_and_take_first = |v: &mut Vec<FileLocation>| {
            v.sort_by(|a, b| {
                a.file_path
                    .cmp(&b.file_path)
                    .then(a.start_line.cmp(&b.start_line))
                    .then(a.start_column.cmp(&b.start_column))
                    .then(a.file_id.cmp(&b.file_id))
            });
            v[0].clone()
        };

        for id in &ids {
            if let Some(v) = scope_locs.get_mut(id) {
                if !v.is_empty() {
                    out.insert(*id, sort_and_take_first(v));
                    continue;
                }
            }
            if let Some(v) = token_locs.get_mut(id) {
                if !v.is_empty() {
                    out.insert(*id, sort_and_take_first(v));
                }
            }
        }
        Ok(out)
    }

    /// Just the symbol-table half: indexed + definition_kind by id.
    fn load_symbol_meta(&self, ids_in: &[i64]) -> Result<HashMap<i64, SymbolMeta>> {
        let mut out = HashMap::new();
        let ids = dedup_preserve_order(ids_in);
        if ids.is_empty() {
            return Ok(out);
        }
        self.db.run_chunked_in(
            "SELECT id, definition_kind FROM symbol WHERE id IN ({IN})",
            &ids,
            &[],
            |row| {
                out.insert(
                    row.get::<_, i64>(0)?,
                    SymbolMeta {
                        indexed: true,
                        definition_kind: row.get::<_, i32>(1)?,
                    },
                );
                Ok(())
            },
            CHUNK_SIZE,
        )?;
        Ok(out)
    }

    /// Which of `ids` are present in `local_symbol` (symhow.file/1's
    /// is_local_symbol field).
    fn load_local_symbol_ids(&self, ids_in: &[i64]) -> Result<HashSet<i64>> {
        let mut out = HashSet::new();
        let ids = dedup_preserve_order(ids_in);
        if ids.is_empty() {
            return Ok(out);
        }
        self.db.run_chunked_in(
            "SELECT id FROM local_symbol WHERE id IN ({IN})",
            &ids,
            &[],
            |row| {
                out.insert(row.get::<_, i64>(0)?);
                Ok(())
            },
            CHUNK_SIZE,
        )?;
        Ok(out)
    }

    /// Batched: one `node` query, one `symbol` query, one occurrence/
    /// source_location query (SCOPE-preferred-over-TOKEN, mirroring
    /// `QueryEngine::definition()`'s per-symbol logic), all chunked.
    /// Duplicate ids in `ids` are harmless (deduped internally); ids with no
    /// `node` row simply get no entry (callers fall back to a default
    /// `HydratedNode`).
    fn hydrate_nodes(&self, ids_in: &[i64]) -> Result<HashMap<i64, HydratedNode>> {
        let mut out = HashMap::new();
        let ids = dedup_preserve_order(ids_in);
        if ids.is_empty() {
            return Ok(out);
        }

        self.db.run_chunked_in(
            "SELECT id, type, serialized_name FROM node WHERE id IN ({IN})",
            &ids,
            &[],
            |row| {
                out.insert(
                    row.get::<_, i64>(0)?,
                    HydratedNode {
                        found: true,
                        kind: row.get::<_, i32>(1)?,
                        serialized_name: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        ..HydratedNode::default()
                    },
                );
                Ok(())
            },
            CHUNK_SIZE,
        )?;

        let sym_meta = self.load_symbol_meta(&ids)?;
        for (id, meta) in &sym_meta {
            if let Some(node) = out.get_mut(id) {
                node.indexed = meta.indexed;
                node.definition_kind = meta.definition_kind;
            }
        }

        let locs = self.load_best_locations(&ids)?;
        for (id, loc) in locs {
            if let Some(node) = out.get_mut(&id) {
                node.location = Some(loc);
            }
        }

        Ok(out)
    }

    // --- node-object builders ----------------------------------------------

    fn location_object(loc: &FileLocation) -> Value {
        json!({
            "file": loc.file_path,
            "start_line": loc.start_line,
            "start_column": loc.start_column,
            "end_line": loc.end_line,
            "end_column": loc.end_column,
            "kind": location_kind_to_string(loc.location_kind),
        })
    }

    /// The canonical node-object builder every schema funnels through.
    /// `name` = the decoded name's bare leaf element (no ancestors, no
    /// signature); `qualified_name` = `DecodedName::name_path()` (full
    /// "A::B::C" ancestor path, no signature); `signature` =
    /// `DecodedName::signature()` (full signature incl. return type).
    /// `display_name` is added only when `DecodedName::display_name()` !=
    /// `DecodedName::qualified_name()` (i.e. only when the hidden TU-local
    /// disambiguator suffix is actually present). `depth`/`seed` are added
    /// only when `depth` has a value (graph verbs only).
    fn node_object(
        &self,
        id: i64,
        kind: i32,
        serialized_name: &str,
        indexed: bool,
        definition_kind: i32,
        location: Option<&FileLocation>,
        depth: Option<i32>,
        seed: bool,
    ) -> Value {
        let decoded = decode_serialized_name(serialized_name);

        let mut j = Map::new();
        j.insert("id".to_string(), json!(id));
        j.insert("kind".to_string(), json!(node_kind_to_string(kind)));
        j.insert(
            "name".to_string(),
            json!(decoded.elements.last().map(|e| e.name.as_str()).unwrap_or("")),
        );
        j.insert("qualified_name".to_string(), json!(decoded.name_path()));
        j.insert("signature".to_string(), json!(decoded.signature()));

        let qualified = decoded.qualified_name();
        let display = decoded.display_name();
        if display != qualified {
            // Only when the hidden TU-local disambiguator suffix was
            // actually present -- see NameDecode's display_name()/
            // qualified_name() docs and docs/JSON_SCHEMA.md.
            j.insert("display_name".to_string(), json!(display));
        }

        j.insert("indexed".to_string(), json!(indexed));
        j.insert(
            "definition_kind".to_string(),
            if indexed {
                json!(definition_kind_to_string(definition_kind))
            } else {
                Value::Null
            },
        );

        if let Some(depth) = depth {
            j.insert("depth".to_string(), json!(depth));
            j.insert("seed".to_string(), json!(seed));
        }

        j.insert(
            "location".to_string(),
            match location {
                Some(loc) => Self::location_object(loc),
                None => Value::Null,
            },
        );
        Value::Object(j)
    }

    fn node_object_from_hydrated(
        &self,
        id: i64,
        n: &HydratedNode,
        depth: Option<i32>,
        seed: bool,
    ) -> Value {
        self.node_object(
            id,
            n.kind,
            &n.serialized_name,
            n.indexed,
            n.definition_kind,
            n.location.as_ref(),
            depth,
            seed,
        )
    }

    /// `ResolvedSymbol` -> node object, per the ambiguous()/not_found() doc
    /// comment on the C++ header (no `node` table re-lookup -- deliberately,
    /// since a file-path candidate's `ResolvedSymbol::serialized_name` holds
    /// the file's `path`, not a real serialized_name, and re-fetching the
    /// real node row for that id could disagree with it).
    fn candidate_node_object(&self, c: &ResolvedSymbol) -> Result<Value> {
        let meta = self.load_symbol_meta(&[c.id])?;
        let loc = self.load_best_locations(&[c.id])?;

        let mut indexed = false;
        let mut definition_kind = 0;
        if let Some(m) = meta.get(&c.id) {
            indexed = m.indexed;
            definition_kind = m.definition_kind;
        }
        let location = loc.get(&c.id).cloned();

        Ok(self.node_object(
            c.id,
            c.kind,
            &c.serialized_name,
            indexed,
            definition_kind,
            location.as_ref(),
            None,
            false,
        ))
    }

    /// Dedups and sorts `ids` by (kind, serialized_name, id) -- the same
    /// tie-break the query layer uses for its node lists (minus `depth`,
    /// which doesn't apply to a plain id set) -- via `hydrated`.
    fn dedup_sorted_by_kind_name(
        &self,
        ids_in: &[i64],
        hydrated: &HashMap<i64, HydratedNode>,
    ) -> Vec<i64> {
        let mut ids = dedup_preserve_order(ids_in);
        ids.sort_by(|&a, &b| {
            let (kind_a, name_a) = hydrated
                .get(&a)
                .map(|h| (h.kind, h.serialized_name.as_str()))
                .unwrap_or((0, ""));
            let (kind_b, name_b) = hydrated
                .get(&b)
                .map(|h| (h.kind, h.serialized_name.as_str()))
                .unwrap_or((0, ""));
            kind_a
                .cmp(&kind_b)
                .then(name_a.cmp(name_b))
                .then(a.cmp(&b))
        });
        ids
    }

    // --- symhow.graph/1 -----------------------------------------------------

    /// symhow.graph/1 -- every graph-producing verb (callers callees bases
    /// derived members neighbors overrides overridden-by context).
    /// `elapsed_ms` is the caller-measured wall time of the `QueryEngine`
    /// call that produced `result` (kept as an explicit parameter, not
    /// measured internally, so callers -- including tests wanting
    /// byte-identical determinism -- fully control it).
    ///
    /// `notes_by_serialized_name`, if given, adds a "notes" field to every
    /// node object: the map value (already the full symhow.note/1-shaped JSON
    /// object, or JSON null) for that node's serialized_name if present, else
    /// JSON null. When `None` (every existing graph verb), no "notes" field
    /// is added at all -- only `symhow query context` passes a map, so
    /// symhow.graph/1's shape for callers/callees/etc. is byte-for-byte
    /// unchanged. Kept as a plain `HashMap<String, Value>` (not a notes-layer
    /// type) so the out layer never depends on the notes store -- the CLI,
    /// which links both, builds this map from the real note objects.
    pub fn graph(
        &self,
        ctx: &QueryContext,
        result: &BfsResult,
        elapsed_ms: i64,
        notes_by_serialized_name: Option<&HashMap<String, Value>>,
    ) -> Result<Value> {
        let mut j = Map::new();
        j.insert("symhow_version".to_string(), json!(self.symhow_version));
        j.insert("schema".to_string(), json!("symhow.graph/1"));

        let mut q = Map::new();
        q.insert("verb".to_string(), json!(ctx.verb));
        q.insert("db".to_string(), json!(ctx.db));
        q.insert("symbol".to_string(), json!(ctx.symbol_or_text_or_path));
        q.insert("symbol_ids".to_string(), json!(ctx.symbol_ids));
        q.insert("depth".to_string(), json!(ctx.depth));
        q.insert(
            "edge_kinds".to_string(),
            Value::Array(
                ctx.edge_kinds
                    .iter()
                    .map(|&k| json!(edge_kind_to_string(k)))
                    .collect(),
            ),
        );
        q.insert(
            "node_kinds".to_string(),
            Value::Array(
                ctx.node_kinds
                    .iter()
                    .map(|&k| json!(node_kind_to_string(k)))
                    .collect(),
            ),
        );
        q.insert("max_nodes".to_string(), json!(ctx.max_nodes));
        j.insert("query".to_string(), Value::Object(q));

        j.insert("truncated".to_string(), json!(result.truncated));

        let mut max_depth = 0;
        for n in &result.nodes {
            max_depth = max_depth.max(n.depth);
        }
        let mut stats = Map::new();
        stats.insert("node_count".to_string(), json!(result.nodes.len()));
        stats.insert("edge_count".to_string(), json!(result.edges.len()));
        stats.insert(
            "levels".to_string(),
            json!(if result.nodes.is_empty() { 0 } else { max_depth }),
        );
        stats.insert("elapsed_ms".to_string(), json!(elapsed_ms));
        j.insert("stats".to_string(), Value::Object(stats));

        let node_ids: Vec<i64> = result.nodes.iter().map(|n| n.id).collect();
        let hydrated = self.hydrate_nodes(&node_ids)?;
        let seed_set: HashSet<i64> = ctx.symbol_ids.iter().copied().collect();

        let mut nodes_json: Vec<Value> = Vec::with_capacity(result.nodes.len());
        for n in &result.nodes {
            let mut node_json;
            let serialized_name_for_notes;
            match hydrated.get(&n.id) {
                Some(h) => {
                    node_json =
                        self.node_object_from_hydrated(n.id, h, Some(n.depth), seed_set.contains(&n.id));
                    serialized_name_for_notes = h.serialized_name.clone();
                }
                None => {
                    // Defensive fallback: the node table lookup somehow
                    // missed a row the BFS itself just hydrated -- fall back
                    // to BFS's own kind/serializedName rather than dropping
                    // the node from output.
                    let fallback = HydratedNode {
                        found: false,
                        kind: n.kind,
                        serialized_name: n.serialized_name.clone(),
                        ..HydratedNode::default()
                    };
                    node_json = self.node_object_from_hydrated(
                        n.id,
                        &fallback,
                        Some(n.depth),
                        seed_set.contains(&n.id),
                    );
                    serialized_name_for_notes = n.serialized_name.clone();
                }
            }
            if let Some(notes) = notes_by_serialized_name {
                let note = notes
                    .get(&serialized_name_for_notes)
                    .cloned()
                    .unwrap_or(Value::Null);
                node_json["notes"] = note;
            }
            nodes_json.push(node_json);
        }
        j.insert("nodes".to_string(), Value::Array(nodes_json));

        let edge_ids: Vec<i64> = result.edges.iter().map(|e| e.id).collect();
        let mut edge_locs: HashMap<i64, Vec<FileLocation>> = HashMap::new();
        if !edge_ids.is_empty() {
            self.db.run_chunked_in(
                "SELECT o.element_id, l.file_node_id, l.start_line, l.start_column, l.end_line, l.end_column, l.type \
                 FROM occurrence o JOIN source_location l ON l.id = o.source_location_id \
                 WHERE o.element_id IN ({IN}) AND l.type IN (0,1,3,9)",
                &dedup_preserve_order(&edge_ids),
                &[],
                |row| {
                    let id = row.get::<_, i64>(0)?;
                    let mut loc = FileLocation::default();
                    loc.file_id = row.get::<_, i64>(1)?;
                    loc.file_path = self
                        .engine
                        .file_map()
                        .get(&loc.file_id)
                        .cloned()
                        .unwrap_or_default();
                    loc.start_line = row.get::<_, i32>(2)?;
                    loc.start_column = row.get::<_, i32>(3)?;
                    loc.end_line = row.get::<_, i32>(4)?;
                    loc.end_column = row.get::<_, i32>(5)?;
                    loc.location_kind = row.get::<_, i32>(6)?;
                    edge_locs.entry(id).or_default().push(loc);
                    Ok(())
                },
                CHUNK_SIZE,
            )?;
            for locs in edge_locs.values_mut() {
                locs.sort_by(|a, b| {
                    a.file_path
                        .cmp(&b.file_path)
                        .then(a.start_line.cmp(&b.start_line))
                        .then(a.start_column.cmp(&b.start_column))
                        .then(a.file_id.cmp(&b.file_id))
                });
            }
        }

        let mut edges_json: Vec<Value> = Vec::with_capacity(result.edges.len());
        for e in &result.edges {
            let mut ej = Map::new();
            ej.insert("id".to_string(), json!(e.id));
            ej.insert("kind".to_string(), json!(edge_kind_to_string(e.kind)));
            ej.insert("source".to_string(), json!(e.source));
            ej.insert("target".to_string(), json!(e.target));
            let locs_json: Vec<Value> = edge_locs
                .get(&e.id)
                .map(|locs| locs.iter().map(Self::location_object).collect())
                .unwrap_or_default();
            ej.insert("locations".to_string(), Value::Array(locs_json));
            edges_json.push(Value::Object(ej));
        }
        j.insert("edges".to_string(), Value::Array(edges_json));

        Ok(Value::Object(j))
    }

    // --- symhow.references/1 --------------------------------------------------

    pub fn references(
        &self,
        ctx: &QueryContext,
        result: &ReferencesResult,
        elapsed_ms: i64,
    ) -> Result<Value> {
        let mut j = Map::new();
        j.insert("symhow_version".to_string(), json!(self.symhow_version));
        j.insert("schema".to_string(), json!("symhow.references/1"));

        let mut q = Map::new();
        q.insert("verb".to_string(), json!(ctx.verb));
        q.insert("db".to_string(), json!(ctx.db));
        q.insert("symbol".to_string(), json!(ctx.symbol_or_text_or_path));
        q.insert("symbol_ids".to_string(), json!(ctx.symbol_ids));
        j.insert("query".to_string(), Value::Object(q));

        // references() has no cap/truncation concept.
        j.insert("truncated".to_string(), json!(false));

        let mut stats = Map::new();
        stats.insert("reference_count".to_string(), json!(result.hits.len()));
        stats.insert("elapsed_ms".to_string(), json!(elapsed_ms));
        j.insert("stats".to_string(), Value::Object(stats));

        let mut ids: Vec<i64> = Vec::with_capacity(result.hits.len() * 2);
        for h in &result.hits {
            ids.push(h.target_node_id);
            ids.push(if h.has_edge {
                h.context_node_id
            } else {
                h.target_node_id
            });
        }
        let hydrated = self.hydrate_nodes(&ids)?;
        let sorted_ids = self.dedup_sorted_by_kind_name(&ids, &hydrated);

        let default_node = HydratedNode::default();
        let mut nodes_json: Vec<Value> = Vec::with_capacity(sorted_ids.len());
        for id in &sorted_ids {
            let n = hydrated.get(id).unwrap_or(&default_node);
            nodes_json.push(self.node_object_from_hydrated(*id, n, None, false));
        }
        j.insert("nodes".to_string(), Value::Array(nodes_json));

        let mut refs_json: Vec<Value> = Vec::with_capacity(result.hits.len());
        for h in &result.hits {
            let mut rj = Map::new();
            rj.insert(
                "edge_id".to_string(),
                if h.has_edge {
                    json!(h.edge_id)
                } else {
                    Value::Null
                },
            );
            // "DECLARATION" is a synthetic label symhow invents -- it is NOT
            // a real edge kind from the schema, just a way to distinguish
            // "this location IS the symbol's own declaration" from "this
            // location is a reference edge pointing at it". See
            // docs/JSON_SCHEMA.md.
            rj.insert(
                "edge_kind".to_string(),
                if h.has_edge {
                    json!(edge_kind_to_string(h.edge_kind))
                } else {
                    json!("DECLARATION")
                },
            );
            rj.insert(
                "context".to_string(),
                json!(if h.has_edge {
                    h.context_node_id
                } else {
                    h.target_node_id
                }),
            );
            rj.insert("target".to_string(), json!(h.target_node_id));
            rj.insert("location".to_string(), Self::location_object(&h.location));
            refs_json.push(Value::Object(rj));
        }
        j.insert("references".to_string(), Value::Array(refs_json));

        Ok(Value::Object(j))
    }

    // --- symhow.definition/1 --------------------------------------------------

    /// symhow.definition/1. Uses `ctx.symbol_ids[0]` as the single "symbol"
    /// object (see docs/JSON_SCHEMA.md for the --all-matches multi-seed
    /// caveat this implies).
    pub fn definition(
        &self,
        ctx: &QueryContext,
        result: &DefinitionResult,
        elapsed_ms: i64,
    ) -> Result<Value> {
        let mut j = Map::new();
        j.insert("symhow_version".to_string(), json!(self.symhow_version));
        j.insert("schema".to_string(), json!("symhow.definition/1"));

        let mut q = Map::new();
        q.insert("verb".to_string(), json!(ctx.verb));
        q.insert("db".to_string(), json!(ctx.db));
        q.insert("symbol".to_string(), json!(ctx.symbol_or_text_or_path));
        q.insert("symbol_ids".to_string(), json!(ctx.symbol_ids));
        j.insert("query".to_string(), Value::Object(q));

        j.insert("truncated".to_string(), json!(false));

        let mut stats = Map::new();
        stats.insert("location_count".to_string(), json!(result.locations.len()));
        stats.insert("elapsed_ms".to_string(), json!(elapsed_ms));
        j.insert("stats".to_string(), Value::Object(stats));

        // Exactly one "symbol" object, not a nodes[] array. With
        // --all-matches (multiple seeds), only the FIRST seed is used here
        // -- result.locations itself is still the merged/sorted set across
        // every seed (an existing QueryEngine::definition() behavior, not
        // new to JSON); see docs/JSON_SCHEMA.md for this caveat.
        if let Some(&sym_id) = ctx.symbol_ids.first() {
            let hydrated = self.hydrate_nodes(&[sym_id])?;
            let default_node = HydratedNode::default();
            let n = hydrated.get(&sym_id).unwrap_or(&default_node);
            j.insert("symbol".to_string(), self.node_object_from_hydrated(sym_id, n, None, false));
        } else {
            j.insert("symbol".to_string(), Value::Null);
        }

        let locs_json: Vec<Value> = result.locations.iter().map(Self::location_object).collect();
        j.insert("locations".to_string(), Value::Array(locs_json));

        Ok(Value::Object(j))
    }

    // --- symhow.search/1 -------------------------------------------------------

    /// symhow.search/1. stats.candidates_scanned (the total row count
    /// search() considered before fuzzy-ranking) is recomputed here via a
    /// direct COUNT(*) matching `QueryEngine::search()`'s own WHERE clause --
    /// that function doesn't return the count itself, and re-deriving it
    /// avoids widening `QueryEngine::search()`'s signature just for this one
    /// JSON field.
    pub fn search(
        &self,
        ctx: &QueryContext,
        hits: &[SearchHit],
        elapsed_ms: i64,
    ) -> Result<Value> {
        let mut j = Map::new();
        j.insert("symhow_version".to_string(), json!(self.symhow_version));
        j.insert("schema".to_string(), json!("symhow.search/1"));

        let mut q = Map::new();
        q.insert("verb".to_string(), json!(ctx.verb));
        q.insert("db".to_string(), json!(ctx.db));
        q.insert("text".to_string(), json!(ctx.symbol_or_text_or_path));
        q.insert("limit".to_string(), json!(ctx.limit));
        q.insert(
            "kinds".to_string(),
            Value::Array(
                ctx.search_kinds
                    .iter()
                    .map(|&k| json!(node_kind_to_string(k)))
                    .collect(),
            ),
        );
        j.insert("query".to_string(), Value::Object(q));

        // search()'s `limit` is an intentional result cap, not a
        // "truncation" in the BFS max_nodes sense -- there's no reliable
        // "were there more matches" signal today without re-running
        // unbounded, so this is always false. See docs/JSON_SCHEMA.md.
        j.insert("truncated".to_string(), json!(false));

        // Mirrors QueryEngine::search()'s own candidate-selection WHERE
        // clause exactly so this count matches what that call actually
        // scanned before fuzzy-ranking.
        let mut candidates_scanned: i64 = 0;
        if ctx.search_kinds.is_empty() {
            self.db.run("SELECT COUNT(*) FROM node", [], |row| {
                candidates_scanned = row.get::<_, i64>(0)?;
                Ok(())
            })?;
        } else {
            let mut mask = 0;
            for &k in &ctx.search_kinds {
                mask |= k;
            }
            self.db.run(
                "SELECT COUNT(*) FROM node WHERE (type & ?) != 0",
                rusqlite::params![mask],
                |row| {
                    candidates_scanned = row.get::<_, i64>(0)?;
                    Ok(())
                },
            )?;
        }

        let mut stats = Map::new();
        stats.insert("candidates_scanned".to_string(), json!(candidates_scanned));
        stats.insert("hit_count".to_string(), json!(hits.len()));
        stats.insert("elapsed_ms".to_string(), json!(elapsed_ms));
        j.insert("stats".to_string(), Value::Object(stats));

        let ids: Vec<i64> = hits.iter().map(|h| h.node_id).collect();
        let hydrated = self.hydrate_nodes(&ids)?;

        let default_node = HydratedNode::default();
        let mut hits_json: Vec<Value> = Vec::with_capacity(hits.len());
        for h in hits {
            let mut hj = Map::new();
            hj.insert("score".to_string(), json!(h.score));
            hj.insert("matched_indices".to_string(), json!(h.matched_indices));
            let n = hydrated.get(&h.node_id).unwrap_or(&default_node);
            hj.insert("node".to_string(), self.node_object_from_hydrated(h.node_id, n, None, false));
            hits_json.push(Value::Object(hj));
        }
        j.insert("hits".to_string(), Value::Array(hits_json));

        Ok(Value::Object(j))
    }

    // --- symhow.stats/1 ---------------------------------------------------------

    /// symhow.stats/1. No elapsed_ms parameter: StatsResult is itself a
    /// handful of COUNT(*)s the CLI doesn't currently time as one unit (see
    /// docs/JSON_SCHEMA.md).
    pub fn stats(&self, ctx: &QueryContext, result: &StatsResult) -> Result<Value> {
        let mut j = Map::new();
        j.insert("symhow_version".to_string(), json!(self.symhow_version));
        j.insert("schema".to_string(), json!("symhow.stats/1"));

        let mut q = Map::new();
        q.insert("verb".to_string(), json!(ctx.verb));
        q.insert("db".to_string(), json!(ctx.db));
        j.insert("query".to_string(), Value::Object(q));

        let size_bytes: i64 = fs::metadata(&ctx.db)
            .map(|m| m.len() as i64)
            .unwrap_or(0);

        let mut database = Map::new();
        database.insert("storage_version".to_string(), json!(result.storage_version));
        database.insert("path".to_string(), json!(ctx.db));
        database.insert("size_bytes".to_string(), json!(size_bytes));
        database.insert(
            "reader_indices_present".to_string(),
            json!(missing_index_names(self.db).is_empty()),
        );
        j.insert("database".to_string(), Value::Object(database));

        let mut files_indexed: i64 = 0;
        self.db.run("SELECT COUNT(*) FROM file WHERE indexed = 1", [], |row| {
            files_indexed = row.get::<_, i64>(0)?;
            Ok(())
        })?;

        let mut counts = Map::new();
        counts.insert("files".to_string(), json!(result.file_count));
        counts.insert("files_indexed".to_string(), json!(files_indexed));
        counts.insert("nodes".to_string(), json!(result.node_count));
        counts.insert("symbols".to_string(), json!(result.symbol_count));
        counts.insert("edges".to_string(), json!(result.edge_count));
        counts.insert("source_locations".to_string(), json!(result.source_location_count));
        counts.insert("occurrences".to_string(), json!(result.occurrence_count));
        counts.insert("errors".to_string(), json!(result.error_count));
        // additive: not in the worked example, kept for completeness
        counts.insert("local_symbols".to_string(), json!(result.local_symbol_count));
        j.insert("counts".to_string(), Value::Object(counts));

        // Sparse: only kinds with count > 0 (already true of nodes_by_kind/
        // edges_by_kind -- see QueryEngine::stats()'s GROUP BY query).
        let mut node_kinds = Map::new();
        for kc in &result.nodes_by_kind {
            node_kinds.insert(node_kind_to_string(kc.kind), json!(kc.count));
        }
        j.insert("node_kinds".to_string(), Value::Object(node_kinds));

        let mut edge_kinds = Map::new();
        for kc in &result.edges_by_kind {
            edge_kinds.insert(edge_kind_to_string(kc.kind), json!(kc.count));
        }
        j.insert("edge_kinds".to_string(), Value::Object(edge_kinds));

        let mut errors: Vec<Value> = Vec::with_capacity(result.errors.len());
        for e in &result.errors {
            let mut ej = Map::new();
            ej.insert("id".to_string(), json!(e.id));
            ej.insert("message".to_string(), json!(e.message));
            ej.insert("fatal".to_string(), json!(e.fatal));
            ej.insert("translation_unit".to_string(), json!(e.translation_unit));
            errors.push(Value::Object(ej));
        }
        j.insert("errors".to_string(), Value::Array(errors));

        Ok(Value::Object(j))
    }

    // --- symhow.file/1 ------------------------------------------------------------

    /// symhow.file/1. `want_contents`/`want_occurrences` mirror the CLI's
    /// --contents/--occurrences flags (FileResult itself already conditions
    /// result.has_contents/result.occurrences on them, but JsonWriter needs
    /// to know whether to emit the "contents"/"occurrences" KEYS at all --
    /// "optional" per the schema means absent-when-not-requested, not
    /// present-with-an-empty-value).
    pub fn file(
        &self,
        ctx: &QueryContext,
        result: &FileResult,
        want_contents: bool,
        want_occurrences: bool,
    ) -> Result<Value> {
        let mut j = Map::new();
        j.insert("symhow_version".to_string(), json!(self.symhow_version));
        j.insert("schema".to_string(), json!("symhow.file/1"));

        let mut q = Map::new();
        q.insert("verb".to_string(), json!(ctx.verb));
        q.insert("db".to_string(), json!(ctx.db));
        q.insert("path".to_string(), json!(ctx.symbol_or_text_or_path));
        j.insert("query".to_string(), Value::Object(q));

        let mut file_obj = Map::new();
        file_obj.insert("id".to_string(), json!(result.file_id));
        file_obj.insert("path".to_string(), json!(result.path));
        file_obj.insert("language".to_string(), json!(result.language));
        file_obj.insert("indexed".to_string(), json!(result.indexed));
        file_obj.insert("complete".to_string(), json!(result.complete));
        file_obj.insert("line_count".to_string(), json!(result.line_count));
        j.insert("file".to_string(), Value::Object(file_obj));

        if want_contents && result.has_contents {
            j.insert("contents".to_string(), json!(result.contents));
        }

        if want_occurrences {
            // Real node-row occurrences get a resolved {kind, qualified_name}
            // (re-decoded from the actual node.serialized_name, not the
            // pre-decoded FileOccurrence::element_name -- which is
            // display_name(), a different string). Zero-kind occurrences
            // (FileOccurrence::element_kind == 0 -- the implicit "not a
            // node row" signal) are resolved into an EXPLICIT
            // is_local_symbol boolean here instead.
            let mut node_element_ids: Vec<i64> = Vec::new();
            let mut zero_kind_element_ids: Vec<i64> = Vec::new();
            for occ in &result.occurrences {
                if occ.element_kind != 0 {
                    node_element_ids.push(occ.element_id);
                } else {
                    zero_kind_element_ids.push(occ.element_id);
                }
            }
            let mut hydrated: HashMap<i64, HydratedNode> = HashMap::new();
            self.db.run_chunked_in(
                "SELECT id, type, serialized_name FROM node WHERE id IN ({IN})",
                &dedup_preserve_order(&node_element_ids),
                &[],
                |row| {
                    hydrated.insert(
                        row.get::<_, i64>(0)?,
                        HydratedNode {
                            found: true,
                            kind: row.get::<_, i32>(1)?,
                            serialized_name: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                            ..HydratedNode::default()
                        },
                    );
                    Ok(())
                },
                CHUNK_SIZE,
            )?;
            let local_symbol_ids = self.load_local_symbol_ids(&zero_kind_element_ids)?;

            let mut occ_arr: Vec<Value> = Vec::with_capacity(result.occurrences.len());
            for occ in &result.occurrences {
                let mut oj = Map::new();
                oj.insert("location".to_string(), Self::location_object(&occ.location));
                if occ.element_kind != 0 {
                    let mut node_obj = Map::new();
                    match hydrated.get(&occ.element_id) {
                        Some(h) => {
                            let decoded = decode_serialized_name(&h.serialized_name);
                            node_obj.insert("kind".to_string(), json!(node_kind_to_string(h.kind)));
                            node_obj.insert("qualified_name".to_string(), json!(decoded.name_path()));
                        }
                        None => {
                            node_obj.insert("kind".to_string(), json!(node_kind_to_string(occ.element_kind)));
                            node_obj.insert("qualified_name".to_string(), json!(occ.element_name));
                        }
                    }
                    oj.insert("node".to_string(), Value::Object(node_obj));
                    oj.insert("is_local_symbol".to_string(), json!(false));
                } else {
                    oj.insert(
                        "is_local_symbol".to_string(),
                        json!(local_symbol_ids.contains(&occ.element_id)),
                    );
                }
                occ_arr.push(Value::Object(oj));
            }
            j.insert("occurrences".to_string(), Value::Array(occ_arr));
        }

        Ok(Value::Object(j))
    }

    // --- symhow.ambiguous/1 / symhow.not-found/1 -------------------------------------

    /// symhow.ambiguous/1 -- printed to stdout (in place of the text
    /// candidate table) before exiting 2, for both symbol ambiguity
    /// (`resolve_symbol()`) and file-path ambiguity (`resolve_file_path()`).
    /// Candidate node objects are built directly from `ResolvedSymbol`'s own
    /// {id, kind, serialized_name} (matching exactly what the existing
    /// text-mode candidate table already renders) rather than re-querying
    /// the `node` table by id -- deliberately, since a file-path candidate's
    /// `ResolvedSymbol::serialized_name` holds the file's `path`, not a real
    /// serialized_name, and re-fetching the real node row for that id could
    /// disagree with it.
    pub fn ambiguous(&self, ctx: &QueryContext, candidates: &[ResolvedSymbol]) -> Result<Value> {
        let mut j = Map::new();
        j.insert("symhow_version".to_string(), json!(self.symhow_version));
        j.insert("schema".to_string(), json!("symhow.ambiguous/1"));

        let mut q = Map::new();
        q.insert("verb".to_string(), json!(ctx.verb));
        q.insert("db".to_string(), json!(ctx.db));
        q.insert("symbol".to_string(), json!(ctx.symbol_or_text_or_path));
        j.insert("query".to_string(), Value::Object(q));

        let mut arr: Vec<Value> = Vec::with_capacity(candidates.len());
        for c in candidates {
            arr.push(self.candidate_node_object(c)?);
        }
        j.insert("candidates".to_string(), Value::Array(arr));

        Ok(Value::Object(j))
    }

    /// symhow.not-found/1 -- symbol case: `hits` from
    /// `QueryEngine::search(spec, None, 5)` exactly as the text-mode "did you
    /// mean" fallback already computes.
    pub fn not_found(&self, ctx: &QueryContext, hits: &[SearchHit]) -> Result<Value> {
        let mut j = Map::new();
        j.insert("symhow_version".to_string(), json!(self.symhow_version));
        j.insert("schema".to_string(), json!("symhow.not-found/1"));

        let mut q = Map::new();
        q.insert("verb".to_string(), json!(ctx.verb));
        q.insert("db".to_string(), json!(ctx.db));
        q.insert("symbol".to_string(), json!(ctx.symbol_or_text_or_path));
        j.insert("query".to_string(), Value::Object(q));

        let ids: Vec<i64> = hits.iter().map(|h| h.node_id).collect();
        let hydrated = self.hydrate_nodes(&ids)?;

        let default_node = HydratedNode::default();
        let mut arr: Vec<Value> = Vec::with_capacity(hits.len());
        for h in hits {
            let n = hydrated.get(&h.node_id).unwrap_or(&default_node);
            arr.push(self.node_object_from_hydrated(h.node_id, n, None, false));
        }
        j.insert("suggestions".to_string(), Value::Array(arr));

        Ok(Value::Object(j))
    }

    /// symhow.not-found/1 -- file-path case: the CLI's `file` verb resolves
    /// suggestions via `fuzzy_match()` against the file map directly (not
    /// `QueryEngine::search()`, which only ever ranks symbol nodes), so it
    /// gets its own method rather than forcing a SearchHit shape onto
    /// FuzzyHit + a path string.
    pub fn not_found_files(
        &self,
        ctx: &QueryContext,
        hits: &[FuzzyHit],
        file_map: &HashMap<i64, String>,
    ) -> Result<Value> {
        let mut j = Map::new();
        j.insert("symhow_version".to_string(), json!(self.symhow_version));
        j.insert("schema".to_string(), json!("symhow.not-found/1"));

        let mut q = Map::new();
        q.insert("verb".to_string(), json!(ctx.verb));
        q.insert("db".to_string(), json!(ctx.db));
        q.insert("symbol".to_string(), json!(ctx.symbol_or_text_or_path));
        j.insert("query".to_string(), Value::Object(q));

        let ids: Vec<i64> = hits.iter().map(|h| h.node_id).collect();
        let locs = self.load_best_locations(&ids)?;

        let mut arr: Vec<Value> = Vec::with_capacity(hits.len());
        for h in hits {
            let path = file_map
                .get(&h.node_id)
                .cloned()
                .unwrap_or_default();
            let loc = locs.get(&h.node_id).cloned();
            arr.push(self.node_object(
                h.node_id,
                node_kind::FILE,
                &path,
                /*indexed=*/ false,
                /*definition_kind=*/ 0,
                loc.as_ref(),
                None,
                false,
            ));
        }
        j.insert("suggestions".to_string(), Value::Array(arr));

        Ok(Value::Object(j))
    }
}
