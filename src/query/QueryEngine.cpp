#include "kg/query/QueryEngine.h"

#include <sqlite3.h>

#include <algorithm>
#include <unordered_set>

#include "kg/db/Enums.h"
#include "kg/db/NameDecode.h"
#include "kg/query/FuzzyMatcher.h"

namespace kg::query
{

namespace
{

std::string columnText(sqlite3_stmt* stmt, int col)
{
    const unsigned char* text = sqlite3_column_text(stmt, col);
    return text != nullptr ? reinterpret_cast<const char*>(text) : std::string();
}

bool columnIsNull(sqlite3_stmt* stmt, int col)
{
    return sqlite3_column_type(stmt, col) == SQLITE_NULL;
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

void sortLocations(std::vector<FileLocation>& locs)
{
    std::sort(locs.begin(), locs.end(), [](const FileLocation& a, const FileLocation& b) {
        if (a.filePath != b.filePath)
        {
            return a.filePath < b.filePath;
        }
        if (a.startLine != b.startLine)
        {
            return a.startLine < b.startLine;
        }
        if (a.startColumn != b.startColumn)
        {
            return a.startColumn < b.startColumn;
        }
        return a.fileId < b.fileId;
    });
}

}  // namespace

QueryEngine::QueryEngine(kg::db::Database& db) : m_db(db)
{
    loadFileMap();
}

void QueryEngine::loadFileMap()
{
    m_db.run("SELECT id, path FROM file", [&](sqlite3_stmt* stmt) {
        m_fileById.emplace(sqlite3_column_int64(stmt, 0), columnText(stmt, 1));
    });
}

std::string QueryEngine::resolveFilePath(int64_t fileId) const
{
    const auto it = m_fileById.find(fileId);
    return it != m_fileById.end() ? it->second : std::string();
}

BfsResult QueryEngine::sortedBfs(BfsParams params) const
{
    BfsResult result = runBfs(m_db, params);
    sortNodes(result.nodes);
    sortEdges(result.edges);
    return result;
}

BfsResult QueryEngine::callers(const std::vector<int64_t>& symbolIds, int depth, int maxNodes, int nodeKindMask) const
{
    return sortedBfs(BfsParams{symbolIds, kg::db::EdgeKind::CALL, /*forward=*/false, depth, maxNodes, nodeKindMask});
}

BfsResult QueryEngine::callees(const std::vector<int64_t>& symbolIds, int depth, int maxNodes, int nodeKindMask) const
{
    return sortedBfs(BfsParams{symbolIds, kg::db::EdgeKind::CALL, /*forward=*/true, depth, maxNodes, nodeKindMask});
}

BfsResult QueryEngine::bases(const std::vector<int64_t>& symbolIds, int depth, int maxNodes, int nodeKindMask) const
{
    // INHERITANCE edges are recorded source = derived class, target = base
    // class (confirmed against the real fixture: e.g. Square -> Polygon,
    // Square -> Colored, Polygon -> Shape, Colored -> Shape), so walking
    // forward (source_node_id IN frontier) walks derived -> base.
    return sortedBfs(BfsParams{symbolIds, kg::db::EdgeKind::INHERITANCE, /*forward=*/true, depth, maxNodes, nodeKindMask});
}

BfsResult QueryEngine::derived(const std::vector<int64_t>& symbolIds, int depth, int maxNodes, int nodeKindMask) const
{
    return sortedBfs(BfsParams{symbolIds, kg::db::EdgeKind::INHERITANCE, /*forward=*/false, depth, maxNodes, nodeKindMask});
}

BfsResult QueryEngine::overrides(const std::vector<int64_t>& symbolIds, int nodeKindMask) const
{
    return sortedBfs(BfsParams{symbolIds, kg::db::EdgeKind::OVERRIDE, /*forward=*/true, 1, 0, nodeKindMask});
}

BfsResult QueryEngine::overriddenBy(const std::vector<int64_t>& symbolIds, int nodeKindMask) const
{
    return sortedBfs(BfsParams{symbolIds, kg::db::EdgeKind::OVERRIDE, /*forward=*/false, 1, 0, nodeKindMask});
}

BfsResult QueryEngine::members(const std::vector<int64_t>& symbolIds, int nodeKindMask) const
{
    return sortedBfs(BfsParams{symbolIds, kg::db::EdgeKind::MEMBER, /*forward=*/true, 1, 0, nodeKindMask});
}

BfsResult QueryEngine::neighbors(const std::vector<int64_t>& symbolIds, int edgeKindMask) const
{
    BfsResult result;
    std::unordered_set<int64_t> seedSet(symbolIds.begin(), symbolIds.end());
    std::unordered_set<int64_t> nodeIds(seedSet.begin(), seedSet.end());
    std::unordered_set<int64_t> seenEdgeIds;

    for (int64_t seed : symbolIds)
    {
        m_db.run(
            "SELECT id,type,source_node_id,target_node_id FROM edge WHERE (source_node_id = ? OR target_node_id = ?) AND (type & ?) != 0",
            [&](sqlite3_stmt* stmt) {
                sqlite3_bind_int64(stmt, 1, seed);
                sqlite3_bind_int64(stmt, 2, seed);
                sqlite3_bind_int(stmt, 3, edgeKindMask);
            },
            [&](sqlite3_stmt* stmt) {
                const int64_t edgeId = sqlite3_column_int64(stmt, 0);
                if (seenEdgeIds.count(edgeId) != 0)
                {
                    return;
                }
                seenEdgeIds.insert(edgeId);
                const int kind = sqlite3_column_int(stmt, 1);
                const int64_t source = sqlite3_column_int64(stmt, 2);
                const int64_t target = sqlite3_column_int64(stmt, 3);
                result.edges.push_back(BfsEdge{edgeId, kind, source, target});
                nodeIds.insert(source);
                nodeIds.insert(target);
            });
    }

    std::vector<int64_t> idsToHydrate(nodeIds.begin(), nodeIds.end());
    m_db.runChunkedIn("SELECT id,type,serialized_name FROM node WHERE id IN ({IN})", idsToHydrate, [&](sqlite3_stmt* stmt) {
        const int64_t id = sqlite3_column_int64(stmt, 0);
        BfsNode node;
        node.id = id;
        node.kind = sqlite3_column_int(stmt, 1);
        node.serializedName = columnText(stmt, 2);
        node.depth = seedSet.count(id) != 0 ? 0 : 1;
        result.nodes.push_back(std::move(node));
    });

    sortNodes(result.nodes);
    sortEdges(result.edges);
    return result;
}

ReferencesResult QueryEngine::references(const std::vector<int64_t>& symbolIds) const
{
    ReferencesResult result;

    for (int64_t symbolId : symbolIds)
    {
        struct EdgeInfo
        {
            int kind;
            int64_t source;
        };
        std::unordered_map<int64_t, EdgeInfo> edgeById;
        m_db.run(
            "SELECT id, type, source_node_id FROM edge WHERE target_node_id = ?",
            [&](sqlite3_stmt* stmt) { sqlite3_bind_int64(stmt, 1, symbolId); },
            [&](sqlite3_stmt* stmt) {
                edgeById.emplace(sqlite3_column_int64(stmt, 0), EdgeInfo{sqlite3_column_int(stmt, 1), sqlite3_column_int64(stmt, 2)});
            });

        std::vector<int64_t> idsToQuery;
        idsToQuery.push_back(symbolId);
        for (const auto& [edgeId, info] : edgeById)
        {
            idsToQuery.push_back(edgeId);
        }

        static constexpr int kRelevantKinds[] = {
            kg::db::LocationKind::TOKEN, kg::db::LocationKind::SCOPE, kg::db::LocationKind::LOCAL_SYMBOL, kg::db::LocationKind::UNSOLVED};

        m_db.runChunkedIn(
            "SELECT o.element_id, l.id, l.file_node_id, l.start_line, l.start_column, l.end_line, l.end_column, l.type "
            "FROM occurrence o JOIN source_location l ON l.id = o.source_location_id "
            "WHERE o.element_id IN ({IN}) AND l.type IN (0,1,3,9)",
            idsToQuery,
            [&](sqlite3_stmt* stmt) {
                const int64_t elementId = sqlite3_column_int64(stmt, 0);

                ReferenceHit hit;
                hit.location.fileId = sqlite3_column_int64(stmt, 2);
                hit.location.filePath = resolveFilePath(hit.location.fileId);
                hit.location.startLine = sqlite3_column_int(stmt, 3);
                hit.location.startColumn = sqlite3_column_int(stmt, 4);
                hit.location.endLine = sqlite3_column_int(stmt, 5);
                hit.location.endColumn = sqlite3_column_int(stmt, 6);
                hit.location.locationKind = sqlite3_column_int(stmt, 7);
                hit.targetNodeId = symbolId;

                const auto it = edgeById.find(elementId);
                if (it != edgeById.end())
                {
                    hit.hasEdge = true;
                    hit.edgeId = elementId;
                    hit.edgeKind = it->second.kind;
                    hit.contextNodeId = it->second.source;
                }
                else
                {
                    hit.hasEdge = false;
                }
                result.hits.push_back(std::move(hit));
            });
        (void)kRelevantKinds;  // documents the l.type filter above; kept as a named constant for clarity
    }

    std::sort(result.hits.begin(), result.hits.end(), [](const ReferenceHit& a, const ReferenceHit& b) {
        if (a.location.filePath != b.location.filePath)
        {
            return a.location.filePath < b.location.filePath;
        }
        if (a.location.startLine != b.location.startLine)
        {
            return a.location.startLine < b.location.startLine;
        }
        if (a.location.startColumn != b.location.startColumn)
        {
            return a.location.startColumn < b.location.startColumn;
        }
        return a.edgeId < b.edgeId;
    });
    return result;
}

DefinitionResult QueryEngine::definition(const std::vector<int64_t>& symbolIds) const
{
    DefinitionResult result;

    for (int64_t symbolId : symbolIds)
    {
        std::vector<FileLocation> scopeLocs;
        std::vector<FileLocation> tokenLocs;

        m_db.run(
            "SELECT l.id, l.file_node_id, l.start_line, l.start_column, l.end_line, l.end_column, l.type "
            "FROM occurrence o JOIN source_location l ON l.id = o.source_location_id "
            "WHERE o.element_id = ? AND l.type IN (0,1)",
            [&](sqlite3_stmt* stmt) { sqlite3_bind_int64(stmt, 1, symbolId); },
            [&](sqlite3_stmt* stmt) {
                FileLocation loc;
                loc.fileId = sqlite3_column_int64(stmt, 1);
                loc.filePath = resolveFilePath(loc.fileId);
                loc.startLine = sqlite3_column_int(stmt, 2);
                loc.startColumn = sqlite3_column_int(stmt, 3);
                loc.endLine = sqlite3_column_int(stmt, 4);
                loc.endColumn = sqlite3_column_int(stmt, 5);
                loc.locationKind = sqlite3_column_int(stmt, 6);
                if (loc.locationKind == kg::db::LocationKind::SCOPE)
                {
                    scopeLocs.push_back(std::move(loc));
                }
                else
                {
                    tokenLocs.push_back(std::move(loc));
                }
            });

        // Prefer SCOPE (full defining body) over TOKEN (bare name span) when
        // both exist for this symbol -- SCOPE is only ever recorded for an
        // actual `clang_isCursorDefinition()` site (see AstWalker.cpp), so
        // its presence means "this IS a definition site", which is strictly
        // more useful for a `definition()` query than the always-present
        // TOKEN span. Only fall back to TOKEN locations (e.g. a
        // forward-declared-only or pure-virtual symbol) when no SCOPE
        // location exists at all.
        std::vector<FileLocation>& chosen = !scopeLocs.empty() ? scopeLocs : tokenLocs;
        for (FileLocation& loc : chosen)
        {
            result.locations.push_back(std::move(loc));
        }
    }

    sortLocations(result.locations);
    return result;
}

std::vector<SearchHit> QueryEngine::search(const std::string& text, const std::vector<int>* kindFilter, int limit) const
{
    int mask = 0;
    if (kindFilter != nullptr)
    {
        for (int k : *kindFilter)
        {
            mask |= k;
        }
    }

    struct Meta
    {
        int kind;
        std::string qualifiedName;
        std::string displayName;
        bool indexed;
    };
    std::unordered_map<int64_t, Meta> meta;
    std::vector<std::pair<int64_t, std::string>> candidates;

    const std::string sql = (kindFilter != nullptr && !kindFilter->empty())
                                 ? "SELECT n.id, n.type, n.serialized_name, s.definition_kind FROM node n LEFT JOIN symbol s ON s.id = "
                                   "n.id WHERE (n.type & ?) != 0"
                                 : "SELECT n.id, n.type, n.serialized_name, s.definition_kind FROM node n LEFT JOIN symbol s ON s.id = n.id";

    m_db.run(
        sql,
        [&](sqlite3_stmt* stmt) {
            if (kindFilter != nullptr && !kindFilter->empty())
            {
                sqlite3_bind_int(stmt, 1, mask);
            }
        },
        [&](sqlite3_stmt* stmt) {
            const int64_t id = sqlite3_column_int64(stmt, 0);
            const int kind = sqlite3_column_int(stmt, 1);
            const std::string sname = columnText(stmt, 2);
            const bool indexed = !columnIsNull(stmt, 3);

            const kg::db::DecodedName decoded = kg::db::decodeSerializedName(sname);
            Meta m;
            m.kind = kind;
            m.qualifiedName = decoded.qualifiedName();
            m.displayName = decoded.displayName();
            m.indexed = indexed;
            meta.emplace(id, m);
            candidates.emplace_back(id, m.displayName);
        });

    const int fuzzyLimit = limit > 0 ? limit * 3 : 0;
    std::vector<FuzzyHit> hits = fuzzyMatch(text, candidates, fuzzyLimit);

    std::vector<SearchHit> out;
    out.reserve(hits.size());
    for (const FuzzyHit& h : hits)
    {
        const auto it = meta.find(h.nodeId);
        if (it == meta.end())
        {
            continue;
        }
        SearchHit sh;
        sh.nodeId = h.nodeId;
        sh.kind = it->second.kind;
        sh.displayName = it->second.displayName;
        sh.qualifiedName = it->second.qualifiedName;
        sh.score = h.score;
        sh.indexed = it->second.indexed;
        sh.matchedIndices = h.matchedIndices;
        out.push_back(std::move(sh));
    }

    std::sort(out.begin(), out.end(), [](const SearchHit& a, const SearchHit& b) {
        if (a.score != b.score)
        {
            return a.score > b.score;
        }
        if (a.displayName.size() != b.displayName.size())
        {
            return a.displayName.size() < b.displayName.size();
        }
        if (a.displayName != b.displayName)
        {
            return a.displayName < b.displayName;
        }
        return a.nodeId < b.nodeId;
    });

    if (limit > 0 && static_cast<int>(out.size()) > limit)
    {
        out.resize(static_cast<std::size_t>(limit));
    }
    return out;
}

FileResult QueryEngine::fileInfo(
    int64_t fileId,
    bool wantContents,
    bool wantOccurrences,
    const std::optional<std::pair<int, int>>& lineRange,
    const std::vector<int>* locationKindFilter) const
{
    FileResult result;
    result.fileId = fileId;

    m_db.run(
        "SELECT path, language, indexed, complete, line_count FROM file WHERE id = ?",
        [&](sqlite3_stmt* stmt) { sqlite3_bind_int64(stmt, 1, fileId); },
        [&](sqlite3_stmt* stmt) {
            result.path = columnText(stmt, 0);
            result.language = columnText(stmt, 1);
            result.indexed = sqlite3_column_int(stmt, 2) != 0;
            result.complete = sqlite3_column_int(stmt, 3) != 0;
            result.lineCount = sqlite3_column_int(stmt, 4);
        });

    if (wantContents)
    {
        m_db.run(
            "SELECT content FROM filecontent WHERE id = ?",
            [&](sqlite3_stmt* stmt) { sqlite3_bind_int64(stmt, 1, fileId); },
            [&](sqlite3_stmt* stmt) {
                result.contents = columnText(stmt, 0);
                result.hasContents = true;
            });
    }

    if (wantOccurrences)
    {
        std::string sql =
            "SELECT id, start_line, start_column, end_line, end_column, type FROM source_location WHERE file_node_id = ?";
        if (lineRange.has_value())
        {
            sql += " AND start_line BETWEEN ? AND ?";
        }
        if (locationKindFilter != nullptr && !locationKindFilter->empty())
        {
            sql += " AND type IN (";
            for (std::size_t i = 0; i < locationKindFilter->size(); ++i)
            {
                if (i != 0)
                {
                    sql += ",";
                }
                sql += std::to_string((*locationKindFilter)[i]);
            }
            sql += ")";
        }

        struct LocRow
        {
            int64_t id;
            FileLocation loc;
        };
        std::vector<LocRow> locRows;
        m_db.run(
            sql,
            [&](sqlite3_stmt* stmt) {
                sqlite3_bind_int64(stmt, 1, fileId);
                if (lineRange.has_value())
                {
                    sqlite3_bind_int(stmt, 2, lineRange->first);
                    sqlite3_bind_int(stmt, 3, lineRange->second);
                }
            },
            [&](sqlite3_stmt* stmt) {
                LocRow row;
                row.id = sqlite3_column_int64(stmt, 0);
                row.loc.fileId = fileId;
                row.loc.filePath = result.path;
                row.loc.startLine = sqlite3_column_int(stmt, 1);
                row.loc.startColumn = sqlite3_column_int(stmt, 2);
                row.loc.endLine = sqlite3_column_int(stmt, 3);
                row.loc.endColumn = sqlite3_column_int(stmt, 4);
                row.loc.locationKind = sqlite3_column_int(stmt, 5);
                locRows.push_back(std::move(row));
            });

        if (!locRows.empty())
        {
            std::vector<int64_t> locIds;
            locIds.reserve(locRows.size());
            for (const LocRow& r : locRows)
            {
                locIds.push_back(r.id);
            }

            std::unordered_map<int64_t, std::vector<int64_t>> elementsByLoc;
            m_db.runChunkedIn(
                "SELECT element_id, source_location_id FROM occurrence WHERE source_location_id IN ({IN})", locIds, [&](sqlite3_stmt* stmt) {
                    elementsByLoc[sqlite3_column_int64(stmt, 1)].push_back(sqlite3_column_int64(stmt, 0));
                });

            std::unordered_set<int64_t> allElementIds;
            for (const auto& [locId, elems] : elementsByLoc)
            {
                for (int64_t e : elems)
                {
                    allElementIds.insert(e);
                }
            }

            struct ElemInfo
            {
                int kind;
                std::string displayName;
            };
            std::unordered_map<int64_t, ElemInfo> elemInfo;
            std::vector<int64_t> elemIdsVec(allElementIds.begin(), allElementIds.end());
            m_db.runChunkedIn("SELECT id,type,serialized_name FROM node WHERE id IN ({IN})", elemIdsVec, [&](sqlite3_stmt* stmt) {
                const int64_t id = sqlite3_column_int64(stmt, 0);
                const int kind = sqlite3_column_int(stmt, 1);
                const std::string sname = columnText(stmt, 2);
                elemInfo[id] = ElemInfo{kind, kg::db::decodeSerializedName(sname).displayName()};
            });

            for (const LocRow& r : locRows)
            {
                const auto it = elementsByLoc.find(r.id);
                if (it == elementsByLoc.end())
                {
                    continue;
                }
                for (int64_t elementId : it->second)
                {
                    FileOccurrence occ;
                    occ.elementId = elementId;
                    occ.location = r.loc;
                    const auto eit = elemInfo.find(elementId);
                    if (eit != elemInfo.end())
                    {
                        occ.elementKind = eit->second.kind;
                        occ.elementName = eit->second.displayName;
                    }
                    else
                    {
                        // element_id refers to an edge or local_symbol row
                        // (not itself a `node`) -- leave kind/name unset;
                        // the CLI/text-writer layer can decide what to show.
                        occ.elementKind = 0;
                    }
                    result.occurrences.push_back(std::move(occ));
                }
            }

            std::sort(result.occurrences.begin(), result.occurrences.end(), [](const FileOccurrence& a, const FileOccurrence& b) {
                if (a.location.startLine != b.location.startLine)
                {
                    return a.location.startLine < b.location.startLine;
                }
                if (a.location.startColumn != b.location.startColumn)
                {
                    return a.location.startColumn < b.location.startColumn;
                }
                return a.elementId < b.elementId;
            });
        }
    }

    return result;
}

StatsResult QueryEngine::stats(int errorLimit) const
{
    StatsResult result;
    result.storageVersion = m_db.storageVersion();

    auto countOf = [&](const char* table) {
        int64_t count = 0;
        m_db.run(std::string("SELECT COUNT(*) FROM ") + table, [&](sqlite3_stmt* stmt) { count = sqlite3_column_int64(stmt, 0); });
        return count;
    };

    result.nodeCount = countOf("node");
    result.edgeCount = countOf("edge");
    result.fileCount = countOf("file");
    result.symbolCount = countOf("symbol");
    result.occurrenceCount = countOf("occurrence");
    result.sourceLocationCount = countOf("source_location");
    result.localSymbolCount = countOf("local_symbol");
    result.errorCount = countOf("error");

    m_db.run("SELECT type, COUNT(*) FROM node GROUP BY type ORDER BY type", [&](sqlite3_stmt* stmt) {
        result.nodesByKind.push_back(KindCount{sqlite3_column_int(stmt, 0), sqlite3_column_int64(stmt, 1)});
    });
    m_db.run("SELECT type, COUNT(*) FROM edge GROUP BY type ORDER BY type", [&](sqlite3_stmt* stmt) {
        result.edgesByKind.push_back(KindCount{sqlite3_column_int(stmt, 0), sqlite3_column_int64(stmt, 1)});
    });

    const int limit = errorLimit > 0 ? errorLimit : 100;
    m_db.run(
        "SELECT id, message, fatal, translation_unit FROM error ORDER BY id LIMIT ?",
        [&](sqlite3_stmt* stmt) { sqlite3_bind_int(stmt, 1, limit); },
        [&](sqlite3_stmt* stmt) {
            StatsErrorRow row;
            row.id = sqlite3_column_int64(stmt, 0);
            row.message = columnText(stmt, 1);
            row.fatal = sqlite3_column_int(stmt, 2) != 0;
            row.translationUnit = columnText(stmt, 3);
            result.errors.push_back(std::move(row));
        });

    return result;
}

std::unordered_map<int64_t, std::string> QueryEngine::primaryLocations(const std::vector<int64_t>& nodeIds) const
{
    struct Best
    {
        int64_t fileId;
        int line;
    };
    std::unordered_map<int64_t, Best> best;

    m_db.runChunkedIn(
        "SELECT o.element_id, l.file_node_id, l.start_line FROM occurrence o JOIN source_location l ON l.id = o.source_location_id "
        "WHERE o.element_id IN ({IN}) AND l.type = 0",
        nodeIds,
        [&](sqlite3_stmt* stmt) {
            const int64_t id = sqlite3_column_int64(stmt, 0);
            const int64_t fileId = sqlite3_column_int64(stmt, 1);
            const int line = sqlite3_column_int(stmt, 2);
            const auto it = best.find(id);
            if (it == best.end() || fileId < it->second.fileId || (fileId == it->second.fileId && line < it->second.line))
            {
                best[id] = Best{fileId, line};
            }
        });

    std::unordered_map<int64_t, std::string> out;
    for (const auto& [id, b] : best)
    {
        out.emplace(id, resolveFilePath(b.fileId) + ":" + std::to_string(b.line));
    }
    return out;
}

}  // namespace kg::query
