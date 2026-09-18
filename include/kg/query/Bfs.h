#pragma once

#include <cstdint>
#include <string>
#include <vector>

#include "kg/db/Database.h"

// The single graph-traversal primitive every QueryEngine graph verb
// (callers/callees/bases/derived/overrides/overriddenBy/members) is built
// on. Clean-room design: one batched SQL query per BFS level (never a
// recursive CTE), a global visited set, and an optional node-kind filter with
// Sourcetrail's own "namespace containment hops pass through transparently"
// behavior. Cost is O(depth) SQL round trips, not O(nodes).
namespace kg::query
{

struct BfsParams
{
    std::vector<int64_t> seeds;
    int edgeKindMask = 0;  // tested via (edge.type & edgeKindMask) != 0
    bool forward = true;   // true: follow source_node_id -> target_node_id (WHERE source_node_id IN frontier)
                            // false: follow target_node_id -> source_node_id (WHERE target_node_id IN frontier)
    int depth = 0;         // 0 = unlimited
    int maxNodes = 0;      // 0 = unlimited; caps result.nodes, sets result.truncated when the cap would be exceeded
    int nodeKindMask = 0;  // 0 = no filtering (all kinds pass); see BfsResult.nodes doc below
};

struct BfsNode
{
    int64_t id = 0;
    int kind = 0;
    std::string serializedName;
    int depth = 0;  // 0 for seeds, else the BFS level at which the node was first discovered
};

struct BfsEdge
{
    int64_t id = 0;
    int kind = 0;
    int64_t source = 0;
    int64_t target = 0;
};

struct BfsResult
{
    // Every edge discovered during the traversal, REGARDLESS of whether its
    // far endpoint passed params.nodeKindMask or the maxNodes cap -- an edge
    // landing back on an already-visited node is how cycles/recursion
    // terminate, and is still meaningful to a caller building a graph
    // picture, so it's always kept.
    std::vector<BfsEdge> edges;

    // Every visited node that either (a) has no nodeKindMask filtering
    // applied to it (seeds, and any traversal with nodeKindMask == 0), or
    // (b) matched nodeKindMask, EXCLUDING nodes that only passed through as
    // "namespace-like" containment hops (see kg::db::isNamespaceLikeNodeKind)
    // -- those still count toward depth/visited/maxNodes bookkeeping and can
    // appear as edge endpoints, but are deliberately left out of this list
    // since the caller asked for e.g. "just classes" and a namespace isn't
    // one, even though the traversal had to pass through it.
    std::vector<BfsNode> nodes;

    bool truncated = false;
};

BfsResult runBfs(kg::db::Database& db, const BfsParams& params);

}  // namespace kg::query
