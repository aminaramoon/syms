#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <unordered_map>
#include <vector>

#include "kg/db/Database.h"
#include "kg/query/Bfs.h"
#include "kg/query/Results.h"

// One method per `kg query` verb. Every graph verb takes a VECTOR of seed
// symbol ids rather than a single one, so both the common case (one
// resolved symbol) and `--all-matches` (every candidate of an Ambiguous
// resolveSymbol() result) are the exact same code path.
//
// Everything here hits SQLite fresh per call (via kg::query::runBfs() or raw
// Database::run()/runChunkedIn() calls) except the file id -> path map, which
// is worth precomputing once per QueryEngine since it's small (one row per
// TU/header actually touched) and looked up constantly while rendering
// locations.
namespace kg::query
{

class QueryEngine
{
public:
    explicit QueryEngine(kg::db::Database& db);

    // `nodeKindMask` (0 = no filtering) is the CLI's `--kind` flag applied to
    // the RESULT of the traversal (see kg::query::BfsParams::nodeKindMask /
    // BfsResult::nodes for the namespace-pass-through semantics) -- separate
    // from `--kind` narrowing WHICH symbol a spec string resolves to, which
    // is SymbolResolver's job, not QueryEngine's.
    BfsResult callers(const std::vector<int64_t>& symbolIds, int depth, int maxNodes, int nodeKindMask = 0) const;
    BfsResult callees(const std::vector<int64_t>& symbolIds, int depth, int maxNodes, int nodeKindMask = 0) const;
    BfsResult bases(const std::vector<int64_t>& symbolIds, int depth, int maxNodes, int nodeKindMask = 0) const;
    BfsResult derived(const std::vector<int64_t>& symbolIds, int depth, int maxNodes, int nodeKindMask = 0) const;
    BfsResult overrides(const std::vector<int64_t>& symbolIds, int nodeKindMask = 0) const;     // depth 1, forward (source = overrider)
    BfsResult overriddenBy(const std::vector<int64_t>& symbolIds, int nodeKindMask = 0) const;  // depth 1, reverse
    BfsResult members(const std::vector<int64_t>& symbolIds, int nodeKindMask = 0) const;       // depth 1, forward

    // Single-hop "expand this node" -- NOT routed through runBfs (Sourcetrail's
    // own expand-one-node mechanism is a plain single query, not a traversal).
    BfsResult neighbors(const std::vector<int64_t>& symbolIds, int edgeKindMask) const;

    // --- non-graph verbs -----------------------------------------------------
    ReferencesResult references(const std::vector<int64_t>& symbolIds) const;
    DefinitionResult definition(const std::vector<int64_t>& symbolIds) const;
    std::vector<SearchHit> search(const std::string& text, const std::vector<int>* kindFilter, int limit) const;
    FileResult fileInfo(
        int64_t fileId,
        bool wantContents,
        bool wantOccurrences,
        const std::optional<std::pair<int, int>>& lineRange,
        const std::vector<int>* locationKindFilter) const;
    StatsResult stats(int errorLimit) const;

    // Best-effort "file:line" string per node id (first TOKEN-kind occurrence
    // found, lowest (file,line) wins for determinism), for text-output
    // rendering of BFS trees. Missing ids are simply absent from the map.
    std::unordered_map<int64_t, std::string> primaryLocations(const std::vector<int64_t>& nodeIds) const;

    // The same id -> path map built once in the constructor, exposed so the
    // CLI can fuzzy-match file paths directly (e.g. the `file` verb's
    // NotFound "did you mean" suggestions) without reaching into raw SQL.
    const std::unordered_map<int64_t, std::string>& fileMap() const
    {
        return m_fileById;
    }

private:
    kg::db::Database& m_db;
    std::unordered_map<int64_t, std::string> m_fileById;  // built once in the constructor

    void loadFileMap();
    std::string resolveFilePath(int64_t fileId) const;

    BfsResult sortedBfs(BfsParams params) const;
};

}  // namespace kg::query
