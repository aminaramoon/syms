//! Human-readable text rendering of `QueryEngine` result types. Mirrors
//! `symhow::out::TextWriter` (`src/out/TextWriter.cpp`). This is a human
//! tool: readability over completeness. Nothing here needs to be
//! machine-parsed -- that's the JSON writer's job (Phase 5).
//!
//! Where the C++ writes to a `std::ostream&`, these functions return the
//! same bytes as a `String` (the future CLI port prints them verbatim).

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::db::enums::{edge_kind_to_string, location_kind_to_string, node_kind_to_string};
use crate::db::name_decode::decode_serialized_name;
use crate::query::{
    BfsResult, DefinitionResult, FileResult, ReferencesResult, SearchHit, StatsResult,
};

/// Just enough of a `symhow notes` note (`symhow::notes::StoredNote`) for
/// text rendering under a `symhow query context` tree node -- kept here
/// (rather than depending on the notes layer) so the out layer never links
/// the notes store; the CLI (which links both) builds this map from the real
/// `StoredNote` objects. Mirrors `symhow::out::NodeNoteText`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeNoteText {
    pub brief: String,
    pub filled: bool,
    pub stale: bool,
    /// Rendered block markdown, used only when `notes_full` is true.
    pub full_markdown: String,
}

fn display_name_of(serialized_name: &str) -> String {
    decode_serialized_name(serialized_name).display_name()
}

// `std::getline` on an istringstream: split on '\n', dropping the final
// empty segment a trailing '\n' would otherwise produce (and yielding no
// lines at all for empty input). Keeps '\r' bytes verbatim, like getline.
fn markdown_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    match text.strip_suffix('\n') {
        Some(stripped) => stripped.split('\n').collect(),
        None => text.split('\n').collect(),
    }
}

/// Renders a BFS/graph result as an indented tree: one line per node,
/// indented by `BfsNode::depth`, showing "[KIND] display name  (file:line)".
/// `locations`, if given, supplies the optional "(file:line)" suffix (see
/// `QueryEngine::primary_locations()`) -- omitted entirely when absent, per
/// node. Edges are summarized as a single count line by default; pass
/// `show_edge_list = true` to also itemize them compactly underneath.
/// `notes` (if given) prints, indented one level further than the node line,
/// a "brief" line with a "[stale]"/"[unfilled]" tag for any node found in
/// the map (nodes absent from the map -- no note -- print nothing extra);
/// when `notes_full` is also true, the node's `full_markdown` is printed
/// (indented) instead of just the brief line.
pub fn write_bfs_result(
    result: &BfsResult,
    locations: &HashMap<i64, String>,
    show_edge_list: bool,
    notes: Option<&HashMap<i64, NodeNoteText>>,
    notes_full: bool,
) -> String {
    let mut out = String::new();
    if result.nodes.is_empty() {
        out.push_str("(no nodes)\n");
    }
    for node in &result.nodes {
        let indent = " ".repeat(node.depth as usize * 2);
        let _ = write!(
            out,
            "{}[{}] {} (#{})",
            indent,
            node_kind_to_string(node.kind),
            display_name_of(&node.serialized_name),
            node.id
        );
        if let Some(location) = locations.get(&node.id) {
            if !location.is_empty() {
                let _ = write!(out, "  ({})", location);
            }
        }
        out.push('\n');

        if let Some(notes) = notes {
            if let Some(note) = notes.get(&node.id) {
                let note_indent = format!("{}    ", indent);
                let tag = if note.stale {
                    " [stale]"
                } else if note.filled {
                    ""
                } else {
                    " [unfilled]"
                };
                if notes_full && !note.full_markdown.is_empty() {
                    let _ = write!(out, "{}note{}:\n", note_indent, tag);
                    for line in markdown_lines(&note.full_markdown) {
                        let _ = write!(out, "{}  {}\n", note_indent, line);
                    }
                } else {
                    let _ = write!(out, "{}brief: {}{}\n", note_indent, note.brief, tag);
                }
            }
        }
    }

    let _ = write!(
        out,
        "{} edge(s){}\n",
        result.edges.len(),
        if result.truncated {
            " [truncated: --max-nodes reached]"
        } else {
            ""
        }
    );
    if result.truncated {
        let _ = write!(out, "... truncated at {} nodes\n", result.nodes.len());
    }
    if show_edge_list {
        for edge in &result.edges {
            let _ = write!(
                out,
                "  #{} --[{}]--> #{}\n",
                edge.source,
                edge_kind_to_string(edge.kind),
                edge.target
            );
        }
    }
    out
}

/// One line per reference hit: "file:line:col  [KIND]  context -> target".
pub fn write_references(result: &ReferencesResult) -> String {
    let mut out = String::new();
    if result.hits.is_empty() {
        out.push_str("(no references)\n");
        return out;
    }
    for hit in &result.hits {
        let _ = write!(
            out,
            "{}:{}:{}  [{}]  ",
            hit.location.file_path,
            hit.location.start_line,
            hit.location.start_column,
            location_kind_to_string(hit.location.location_kind)
        );
        if hit.has_edge {
            let _ = write!(
                out,
                "{}  #{} -> #{}",
                edge_kind_to_string(hit.edge_kind),
                hit.context_node_id,
                hit.target_node_id
            );
        } else {
            let _ = write!(out, "declaration  #{}", hit.target_node_id);
        }
        out.push('\n');
    }
    out
}

/// One line per location.
pub fn write_definition(result: &DefinitionResult) -> String {
    let mut out = String::new();
    if result.locations.is_empty() {
        out.push_str("(no definition found)\n");
        return out;
    }
    for loc in &result.locations {
        let _ = write!(
            out,
            "{}:{}:{}-{}:{}  [{}]\n",
            loc.file_path,
            loc.start_line,
            loc.start_column,
            loc.end_line,
            loc.end_column,
            location_kind_to_string(loc.location_kind)
        );
    }
    out
}

/// One line per hit: "score  [KIND]  displayName  (qualifiedName)".
pub fn write_search_hits(hits: &[SearchHit]) -> String {
    let mut out = String::new();
    if hits.is_empty() {
        out.push_str("(no matches)\n");
        return out;
    }
    for hit in hits {
        let _ = write!(
            out,
            "{:>4}  [{}]  {}  ({}){}",
            hit.score,
            node_kind_to_string(hit.kind),
            hit.display_name,
            hit.qualified_name,
            if hit.indexed { "" } else { "  [not indexed]" }
        );
        out.push('\n');
    }
    out
}

/// Key:value block for the file's own metadata, then (if requested) file
/// contents and/or a one-line-per-occurrence listing.
pub fn write_file_result(result: &FileResult) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "path: {}", result.path);
    let _ = writeln!(out, "id: {}", result.file_id);
    let _ = writeln!(out, "language: {}", result.language);
    let _ = writeln!(out, "indexed: {}", if result.indexed { "true" } else { "false" });
    let _ = writeln!(out, "complete: {}", if result.complete { "true" } else { "false" });
    let _ = writeln!(out, "line_count: {}", result.line_count);

    if result.has_contents {
        let _ = write!(out, "--- contents ---\n{}\n", result.contents);
    }

    if !result.occurrences.is_empty() {
        let _ = write!(out, "--- occurrences ({}) ---\n", result.occurrences.len());
        for occ in &result.occurrences {
            let _ = write!(
                out,
                "{}:{}  ",
                occ.location.start_line, occ.location.start_column
            );
            if occ.element_kind != 0 {
                let _ = write!(
                    out,
                    "[{}] {}",
                    node_kind_to_string(occ.element_kind),
                    occ.element_name
                );
            } else {
                let _ = write!(out, "(non-node element #{})", occ.element_id);
            }
            out.push('\n');
        }
    }
    out
}

/// Key:value block: table row counts, node/edge counts by kind, then (if
/// any) the error table.
pub fn write_stats(result: &StatsResult) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "storage_version: {}", result.storage_version);
    let _ = writeln!(out, "nodes: {}", result.node_count);
    let _ = writeln!(out, "edges: {}", result.edge_count);
    let _ = writeln!(out, "files: {}", result.file_count);
    let _ = writeln!(out, "symbols: {}", result.symbol_count);
    let _ = writeln!(out, "occurrences: {}", result.occurrence_count);
    let _ = writeln!(out, "source_locations: {}", result.source_location_count);
    let _ = writeln!(out, "local_symbols: {}", result.local_symbol_count);
    let _ = writeln!(out, "errors: {}", result.error_count);

    out.push_str("--- nodes by kind ---\n");
    for kc in &result.nodes_by_kind {
        let _ = writeln!(out, "  {}: {}", node_kind_to_string(kc.kind), kc.count);
    }
    out.push_str("--- edges by kind ---\n");
    for kc in &result.edges_by_kind {
        let _ = writeln!(out, "  {}: {}", edge_kind_to_string(kc.kind), kc.count);
    }

    if !result.errors.is_empty() {
        let _ = write!(out, "--- errors ({} shown) ---\n", result.errors.len());
        for e in &result.errors {
            let _ = write!(
                out,
                "  #{}{}{}: {}\n",
                e.id,
                if e.fatal { " [FATAL] " } else { " " },
                e.translation_unit,
                e.message
            );
        }
    }
    out
}

/// Shared ambiguous/ "did you mean" candidate table renderer, used by the
/// CLI for `resolve_symbol()`/`resolve_file_path()` Ambiguous results (exit
/// code 2) and fuzzy-suggestion NotFound fallback (exit code 3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CandidateRow {
    pub id: i64,
    pub kind: String,
    pub name: String,
    /// "" if unknown/not looked up.
    pub location: String,
}

pub fn write_candidate_table(rows: &[CandidateRow]) -> String {
    let mut out = String::new();
    for row in rows {
        let _ = write!(out, "  #{}  [{}]  {}", row.id, row.kind, row.name);
        if !row.location.is_empty() {
            let _ = write!(out, "  ({})", row.location);
        }
        out.push('\n');
    }
    out
}
