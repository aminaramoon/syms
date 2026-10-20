#pragma once

#include <cstdint>
#include <ostream>
#include <string>
#include <unordered_map>
#include <vector>

#include "symhow/query/Bfs.h"
#include "symhow/query/Results.h"

// Human-readable text rendering of QueryEngine result types. This is a
// human tool: readability over completeness. Nothing here needs to be
// machine-parsed -- that's the JSON writer's job (Phase 5).
namespace symhow::out
{

// Just enough of a `symhow notes` note (symhow::notes::StoredNote) for text
// rendering under a `symhow query context` tree node -- kept here (rather than
// including symhow/notes/NotesStore.h) so symhow_out never links symhow_notes; the CLI
// (which links both) builds this map from the real StoredNote objects.
struct NodeNoteText
{
    std::string brief;
    bool filled = false;
    bool stale = false;
    std::string fullMarkdown;  // rendered block markdown, used only when notesFull is true
};

// Renders a BFS/graph result as an indented tree: one line per node,
// indented by BfsNode::depth, showing "[KIND] display name  (file:line)".
// `locations`, if given, supplies the optional "(file:line)" suffix (see
// QueryEngine::primaryLocations()) -- omitted entirely when absent, per node.
// Edges are summarized as a single count line by default; pass
// `showEdgeList = true` to also itemize them compactly underneath (kept
// separate from the tree since a graph's edge list rarely reads well
// interleaved with a node tree).
// `notes` (if non-null) prints, indented one level further than the node
// line, a "brief" line with a "[stale]"/"[unfilled]" tag for any node found
// in the map (nodes absent from the map -- no note -- print nothing extra);
// when `notesFull` is also true, the node's `fullMarkdown` is printed
// (indented) instead of just the brief line. Used by `symhow query context`'s
// text mode; every other graph verb passes `notes == nullptr`.
void writeBfsResult(
    std::ostream& out,
    const symhow::query::BfsResult& result,
    const std::unordered_map<int64_t, std::string>& locations = {},
    bool showEdgeList = false,
    const std::unordered_map<int64_t, NodeNoteText>* notes = nullptr,
    bool notesFull = false);

// One line per reference hit: "kind  file:line:col  context -> target".
void writeReferences(std::ostream& out, const symhow::query::ReferencesResult& result);

// One line per location.
void writeDefinition(std::ostream& out, const symhow::query::DefinitionResult& result);

// One line per hit: "score  [KIND]  displayName  (qualifiedName)".
void writeSearchHits(std::ostream& out, const std::vector<symhow::query::SearchHit>& hits);

// Key:value block for the file's own metadata, then (if requested)
// file contents and/or a one-line-per-occurrence listing.
void writeFileResult(std::ostream& out, const symhow::query::FileResult& result);

// Key:value block: table row counts, node/edge counts by kind, then (if any)
// the error table.
void writeStats(std::ostream& out, const symhow::query::StatsResult& result);

// Shared ambiguous/ "did you mean" candidate table renderer, used by the CLI
// for resolveSymbol()/resolveFilePath() Ambiguous results (exit code 2) and
// fuzzy-suggestion NotFound fallback (exit code 3).
struct CandidateRow
{
    int64_t id = 0;
    std::string kind;
    std::string name;
    std::string location;  // "" if unknown/not looked up
};
void writeCandidateTable(std::ostream& out, const std::vector<CandidateRow>& rows);

}  // namespace symhow::out
