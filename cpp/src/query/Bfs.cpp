#include "symhow/query/Bfs.h"

#include <sqlite3.h>

#include <algorithm>
#include <unordered_map>
#include <unordered_set>

#include "symhow/db/Enums.h"

namespace symhow::query
{

namespace
{

struct EdgeRow
{
    int64_t id;
    int kind;
    int64_t source;
    int64_t target;
};

std::string columnText(sqlite3_stmt* stmt, int col)
{
    const unsigned char* text = sqlite3_column_text(stmt, col);
    return text != nullptr ? reinterpret_cast<const char*>(text) : std::string();
}

void sortNodes(std::vector<BfsNode>& nodes)
{
    std::sort(nodes.begin(), nodes.end(), [](const BfsNode& a, const BfsNode& b) {
        if (a.depth != b.depth)
        {
            return a.depth < b.depth;
        }
        if (a.kind != b.kind)
        {
            return a.kind < b.kind;
        }
        if (a.serializedName != b.serializedName)
        {
            return a.serializedName < b.serializedName;
        }
        return a.id < b.id;
    });
}

void sortEdges(std::vector<BfsEdge>& edges)
{
    std::sort(edges.begin(), edges.end(), [](const BfsEdge& a, const BfsEdge& b) {
        if (a.kind != b.kind)
        {
            return a.kind < b.kind;
        }
        if (a.source != b.source)
        {
            return a.source < b.source;
        }
        if (a.target != b.target)
        {
            return a.target < b.target;
        }
        return a.id < b.id;
    });
}

}  // namespace

BfsResult runBfs(symhow::db::Database& db, const BfsParams& params)
{
    BfsResult result;
    if (params.seeds.empty())
    {
        return result;
    }

    std::unordered_set<int64_t> visited(params.seeds.begin(), params.seeds.end());
    std::unordered_map<int64_t, int> depthOf;
    for (int64_t s : params.seeds)
    {
        depthOf[s] = 0;
    }
    // Nodes that are visited/traversed-through but excluded from the
    // rendered node list (namespace-like pass-through under a node-kind
    // filter -- see BfsResult::nodes).
    std::unordered_set<int64_t> suppressedFromRender;

    std::unordered_set<int64_t> emittedEdgeIds;
    std::vector<int64_t> frontier(visited.begin(), visited.end());

    const std::string edgeSql = params.forward
                                     ? "SELECT id,type,source_node_id,target_node_id FROM edge WHERE source_node_id IN ({IN}) AND "
                                       "(type & ?) != 0"
                                     : "SELECT id,type,source_node_id,target_node_id FROM edge WHERE target_node_id IN ({IN}) AND "
                                       "(type & ?) != 0";

    int level = 0;
    while (!frontier.empty() && (params.depth == 0 || level < params.depth))
    {
        std::vector<EdgeRow> rows;
        db.runChunkedIn(
            edgeSql,
            frontier,
            [&](sqlite3_stmt* stmt, int idx) { sqlite3_bind_int(stmt, idx, params.edgeKindMask); },
            [&](sqlite3_stmt* stmt) {
                rows.push_back(EdgeRow{
                    sqlite3_column_int64(stmt, 0),
                    sqlite3_column_int(stmt, 1),
                    sqlite3_column_int64(stmt, 2),
                    sqlite3_column_int64(stmt, 3)});
            });

        std::unordered_set<int64_t> nextFrontier;
        for (const EdgeRow& row : rows)
        {
            if (emittedEdgeIds.count(row.id) == 0)
            {
                emittedEdgeIds.insert(row.id);
                result.edges.push_back(BfsEdge{row.id, row.kind, row.source, row.target});
            }

            const int64_t newNode = params.forward ? row.target : row.source;
            if (visited.count(newNode) == 0)
            {
                nextFrontier.insert(newNode);
            }
        }

        if (params.nodeKindMask != 0 && !nextFrontier.empty())
        {
            std::vector<int64_t> candidates(nextFrontier.begin(), nextFrontier.end());
            std::unordered_map<int64_t, int> kindOf;
            db.runChunkedIn("SELECT id,type FROM node WHERE id IN ({IN})", candidates, [&](sqlite3_stmt* stmt) {
                kindOf[sqlite3_column_int64(stmt, 0)] = sqlite3_column_int(stmt, 1);
            });

            std::unordered_set<int64_t> filtered;
            for (int64_t id : candidates)
            {
                const auto it = kindOf.find(id);
                const int kind = it != kindOf.end() ? it->second : 0;
                if ((kind & params.nodeKindMask) != 0)
                {
                    filtered.insert(id);
                }
                else if (symhow::db::isNamespaceLikeNodeKind(kind))
                {
                    filtered.insert(id);
                    suppressedFromRender.insert(id);
                }
                // else: dropped entirely -- doesn't advance the frontier.
            }
            nextFrontier = std::move(filtered);
        }

        if (params.maxNodes != 0)
        {
            const std::size_t allowed = visited.size() >= static_cast<std::size_t>(params.maxNodes)
                                             ? 0
                                             : static_cast<std::size_t>(params.maxNodes) - visited.size();
            if (nextFrontier.size() > allowed)
            {
                std::vector<int64_t> sortedCandidates(nextFrontier.begin(), nextFrontier.end());
                std::sort(sortedCandidates.begin(), sortedCandidates.end());
                sortedCandidates.resize(allowed);
                nextFrontier = std::unordered_set<int64_t>(sortedCandidates.begin(), sortedCandidates.end());
                result.truncated = true;

                for (int64_t id : nextFrontier)
                {
                    depthOf[id] = level + 1;
                }
                visited.insert(nextFrontier.begin(), nextFrontier.end());
                level += 1;
                break;
            }
        }

        for (int64_t id : nextFrontier)
        {
            depthOf[id] = level + 1;
        }
        visited.insert(nextFrontier.begin(), nextFrontier.end());
        frontier.assign(nextFrontier.begin(), nextFrontier.end());
        level += 1;
    }

    std::vector<int64_t> toHydrate;
    toHydrate.reserve(visited.size());
    for (int64_t id : visited)
    {
        if (suppressedFromRender.count(id) == 0)
        {
            toHydrate.push_back(id);
        }
    }

    db.runChunkedIn("SELECT id,type,serialized_name FROM node WHERE id IN ({IN})", toHydrate, [&](sqlite3_stmt* stmt) {
        const int64_t id = sqlite3_column_int64(stmt, 0);
        BfsNode node;
        node.id = id;
        node.kind = sqlite3_column_int(stmt, 1);
        node.serializedName = columnText(stmt, 2);
        const auto it = depthOf.find(id);
        node.depth = it != depthOf.end() ? it->second : 0;
        result.nodes.push_back(std::move(node));
    });

    sortNodes(result.nodes);
    sortEdges(result.edges);
    return result;
}

}  // namespace symhow::query
