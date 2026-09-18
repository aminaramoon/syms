#pragma once

#include <cstdint>
#include <string>
#include <vector>

// Value-type result structs for every kg::query::QueryEngine verb. Field
// names are chosen to be sensible for BOTH the text writer (this phase) and
// a future JSON writer (Phase 5) -- plain aggregates, no pointers/references,
// nothing that isn't trivially serializable.
namespace kg::query
{

struct FileLocation
{
    int64_t fileId = 0;
    std::string filePath;
    int startLine = 0;
    int startColumn = 0;
    int endLine = 0;
    int endColumn = 0;
    int locationKind = 0;  // kg::db::LocationKind value
};

// One row of `references(symbolId)`: either the symbol's own declaration/
// definition site (hasEdge == false, edgeId/edgeKind/contextNodeId unset)
// or one occurrence of some edge that targets the symbol (hasEdge == true).
struct ReferenceHit
{
    bool hasEdge = false;
    int64_t edgeId = 0;
    int edgeKind = 0;
    int64_t contextNodeId = 0;  // the referencing symbol (edge.source_node_id); 0 if !hasEdge
    int64_t targetNodeId = 0;   // the symbol references() was called on
    FileLocation location;
};

struct ReferencesResult
{
    std::vector<ReferenceHit> hits;
};

struct DefinitionResult
{
    std::vector<FileLocation> locations;
};

struct SearchHit
{
    int64_t nodeId = 0;
    int kind = 0;
    std::string displayName;
    std::string qualifiedName;
    int score = 0;
    bool indexed = true;  // false when node.id has no `symbol` row (definition_kind IS NULL)

    // Character indices (into the ORIGINAL, non-lowercased candidate text --
    // see kg::query::FuzzyHit::matchedIndices) that the fuzzy matcher
    // highlighted for this hit. Phase 4's search() computed this internally
    // (via fuzzyMatch()) but never surfaced it on SearchHit since TextWriter
    // doesn't render per-character highlighting; Phase 5's JSON `search`
    // schema wants it verbatim (matched_indices), so it's carried through
    // here rather than reconstructed some other way.
    std::vector<int> matchedIndices;
};

struct FileOccurrence
{
    int64_t elementId = 0;
    int elementKind = 0;
    std::string elementName;  // decoded displayName() of the element, or empty if it's an edge/local-symbol id
    FileLocation location;
};

struct FileResult
{
    int64_t fileId = 0;
    std::string path;
    std::string language;
    bool indexed = false;
    bool complete = false;
    int lineCount = 0;

    bool hasContents = false;
    std::string contents;

    std::vector<FileOccurrence> occurrences;
};

struct KindCount
{
    int kind = 0;
    int64_t count = 0;
};

struct StatsErrorRow
{
    int64_t id = 0;
    std::string message;
    bool fatal = false;
    std::string translationUnit;
};

struct StatsResult
{
    int storageVersion = 0;

    int64_t nodeCount = 0;
    int64_t edgeCount = 0;
    int64_t fileCount = 0;
    int64_t symbolCount = 0;
    int64_t occurrenceCount = 0;
    int64_t sourceLocationCount = 0;
    int64_t localSymbolCount = 0;
    int64_t errorCount = 0;

    std::vector<KindCount> nodesByKind;
    std::vector<KindCount> edgesByKind;
    std::vector<StatsErrorRow> errors;
};

}  // namespace kg::query
