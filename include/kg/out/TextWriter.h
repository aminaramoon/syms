#pragma once

#include <cstdint>
#include <ostream>
#include <string>
#include <unordered_map>
#include <vector>

#include "kg/query/Bfs.h"
#include "kg/query/Results.h"

// Human-readable text rendering of QueryEngine result types. This is a
// human tool: readability over completeness. Nothing here needs to be
// machine-parsed -- that's the JSON writer's job (Phase 5).
namespace kg::out
{

// Renders a BFS/graph result as an indented tree: one line per node,
// indented by BfsNode::depth, showing "[KIND] display name  (file:line)".
// `locations`, if given, supplies the optional "(file:line)" suffix (see
// QueryEngine::primaryLocations()) -- omitted entirely when absent, per node.
// Edges are summarized as a single count line by default; pass
// `showEdgeList = true` to also itemize them compactly underneath (kept
// separate from the tree since a graph's edge list rarely reads well
// interleaved with a node tree).
void writeBfsResult(std::ostream& out, const kg::query::BfsResult& result, const std::unordered_map<int64_t, std::string>& locations = {}, bool showEdgeList = false);

// One line per reference hit: "kind  file:line:col  context -> target".
void writeReferences(std::ostream& out, const kg::query::ReferencesResult& result);

// One line per location.
void writeDefinition(std::ostream& out, const kg::query::DefinitionResult& result);

// One line per hit: "score  [KIND]  displayName  (qualifiedName)".
void writeSearchHits(std::ostream& out, const std::vector<kg::query::SearchHit>& hits);

// Key:value block for the file's own metadata, then (if requested)
// file contents and/or a one-line-per-occurrence listing.
void writeFileResult(std::ostream& out, const kg::query::FileResult& result);

// Key:value block: table row counts, node/edge counts by kind, then (if any)
// the error table.
void writeStats(std::ostream& out, const kg::query::StatsResult& result);

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

}  // namespace kg::out
