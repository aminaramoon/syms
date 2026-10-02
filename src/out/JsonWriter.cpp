#include "kg/out/JsonWriter.h"

#include <sqlite3.h>

#include <algorithm>
#include <filesystem>

#include "kg/db/Enums.h"
#include "kg/db/NameDecode.h"

namespace kg::out::json
{

namespace
{

std::string columnText(sqlite3_stmt* stmt, int col)
{
    const unsigned char* text = sqlite3_column_text(stmt, col);
    return text != nullptr ? reinterpret_cast<const char*>(text) : std::string();
}

// De-duplicates while preserving first-seen order (used before batched IN
// queries -- purely a size optimization, doesn't affect correctness).
std::vector<int64_t> dedupPreserveOrder(const std::vector<int64_t>& ids)
{
    std::vector<int64_t> out;
    out.reserve(ids.size());
    std::unordered_set<int64_t> seen;
    for (int64_t id : ids)
    {
        if (seen.insert(id).second)
        {
            out.push_back(id);
        }
    }
    return out;
}

}  // namespace

std::vector<int> decomposeEdgeKindMask(int mask)
{
    // Mirrors src/db/Enums.cpp's kEdgeKindTable (minus UNKNOWN/ALL, which
    // aren't individually-meaningful "kinds" for this purpose) -- see the
    // comment on this function in JsonWriter.h for why it's re-listed here
    // rather than shared from Enums.cpp.
    static constexpr int kAllBits[] = {
        kg::db::EdgeKind::MEMBER,
        kg::db::EdgeKind::TYPE_USAGE,
        kg::db::EdgeKind::USAGE,
        kg::db::EdgeKind::CALL,
        kg::db::EdgeKind::INHERITANCE,
        kg::db::EdgeKind::OVERRIDE,
        kg::db::EdgeKind::TYPE_ARGUMENT,
        kg::db::EdgeKind::TEMPLATE_SPECIALIZATION,
        kg::db::EdgeKind::INCLUDE,
        kg::db::EdgeKind::IMPORT,
        kg::db::EdgeKind::MACRO_USAGE,
        kg::db::EdgeKind::ANNOTATION_USAGE,
    };
    std::vector<int> out;
    for (int bit : kAllBits)
    {
        if ((bit & mask) != 0)
        {
            out.push_back(bit);
        }
    }
    return out;
}

JsonWriter::JsonWriter(kg::db::Database& db, kg::query::QueryEngine& engine, std::string kgVersion)
    : m_db(db), m_engine(engine), m_kgVersion(std::move(kgVersion))
{
}

std::unordered_map<int64_t, kg::query::FileLocation> JsonWriter::loadBestLocations(const std::vector<int64_t>& idsIn) const
{
    std::unordered_map<int64_t, kg::query::FileLocation> out;
    const std::vector<int64_t> ids = dedupPreserveOrder(idsIn);
    if (ids.empty())
    {
        return out;
    }

    // Same SCOPE-preferred-over-TOKEN selection as QueryEngine::definition(),
    // batched across every id at once instead of one query per id.
    std::unordered_map<int64_t, std::vector<kg::query::FileLocation>> scopeLocs;
    std::unordered_map<int64_t, std::vector<kg::query::FileLocation>> tokenLocs;

    m_db.runChunkedIn(
        "SELECT o.element_id, l.file_node_id, l.start_line, l.start_column, l.end_line, l.end_column, l.type "
        "FROM occurrence o JOIN source_location l ON l.id = o.source_location_id "
        "WHERE o.element_id IN ({IN}) AND l.type IN (0,1)",
        ids,
        [&](sqlite3_stmt* stmt) {
            const int64_t id = sqlite3_column_int64(stmt, 0);
            kg::query::FileLocation loc;
            loc.fileId = sqlite3_column_int64(stmt, 1);
            loc.filePath = m_engine.fileMap().count(loc.fileId) != 0 ? m_engine.fileMap().at(loc.fileId) : std::string();
            loc.startLine = sqlite3_column_int(stmt, 2);
            loc.startColumn = sqlite3_column_int(stmt, 3);
            loc.endLine = sqlite3_column_int(stmt, 4);
            loc.endColumn = sqlite3_column_int(stmt, 5);
            loc.locationKind = sqlite3_column_int(stmt, 6);
            if (loc.locationKind == kg::db::LocationKind::SCOPE)
            {
                scopeLocs[id].push_back(std::move(loc));
            }
            else
            {
                tokenLocs[id].push_back(std::move(loc));
            }
        });

    auto sortAndTakeFirst = [](std::vector<kg::query::FileLocation>& v) -> kg::query::FileLocation {
        std::sort(v.begin(), v.end(), [](const kg::query::FileLocation& a, const kg::query::FileLocation& b) {
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
        return v.front();
    };

    for (int64_t id : ids)
    {
        auto sIt = scopeLocs.find(id);
        if (sIt != scopeLocs.end() && !sIt->second.empty())
        {
            out.emplace(id, sortAndTakeFirst(sIt->second));
            continue;
        }
        auto tIt = tokenLocs.find(id);
        if (tIt != tokenLocs.end() && !tIt->second.empty())
        {
            out.emplace(id, sortAndTakeFirst(tIt->second));
        }
    }
    return out;
}

std::unordered_map<int64_t, JsonWriter::SymbolMeta> JsonWriter::loadSymbolMeta(const std::vector<int64_t>& idsIn) const
{
    std::unordered_map<int64_t, SymbolMeta> out;
    const std::vector<int64_t> ids = dedupPreserveOrder(idsIn);
    if (ids.empty())
    {
        return out;
    }
    m_db.runChunkedIn("SELECT id, definition_kind FROM symbol WHERE id IN ({IN})", ids, [&](sqlite3_stmt* stmt) {
        SymbolMeta meta;
        meta.indexed = true;
        meta.definitionKind = sqlite3_column_int(stmt, 1);
        out[sqlite3_column_int64(stmt, 0)] = meta;
    });
    return out;
}

std::unordered_set<int64_t> JsonWriter::loadLocalSymbolIds(const std::vector<int64_t>& idsIn) const
{
    std::unordered_set<int64_t> out;
    const std::vector<int64_t> ids = dedupPreserveOrder(idsIn);
    if (ids.empty())
    {
        return out;
    }
    m_db.runChunkedIn(
        "SELECT id FROM local_symbol WHERE id IN ({IN})", ids, [&](sqlite3_stmt* stmt) { out.insert(sqlite3_column_int64(stmt, 0)); });
    return out;
}

std::unordered_map<int64_t, JsonWriter::HydratedNode> JsonWriter::hydrateNodes(const std::vector<int64_t>& idsIn) const
{
    std::unordered_map<int64_t, HydratedNode> out;
    const std::vector<int64_t> ids = dedupPreserveOrder(idsIn);
    if (ids.empty())
    {
        return out;
    }

    m_db.runChunkedIn("SELECT id, type, serialized_name FROM node WHERE id IN ({IN})", ids, [&](sqlite3_stmt* stmt) {
        HydratedNode node;
        node.found = true;
        node.kind = sqlite3_column_int(stmt, 1);
        node.serializedName = columnText(stmt, 2);
        out[sqlite3_column_int64(stmt, 0)] = std::move(node);
    });

    const std::unordered_map<int64_t, SymbolMeta> symMeta = loadSymbolMeta(ids);
    for (const auto& [id, meta] : symMeta)
    {
        auto it = out.find(id);
        if (it != out.end())
        {
            it->second.indexed = meta.indexed;
            it->second.definitionKind = meta.definitionKind;
        }
    }

    const std::unordered_map<int64_t, kg::query::FileLocation> locs = loadBestLocations(ids);
    for (const auto& [id, loc] : locs)
    {
        auto it = out.find(id);
        if (it != out.end())
        {
            it->second.location = loc;
        }
    }

    return out;
}

nlohmann::json JsonWriter::locationObject(const kg::query::FileLocation& loc)
{
    nlohmann::json j;
    j["file"] = loc.filePath;
    j["start_line"] = loc.startLine;
    j["start_column"] = loc.startColumn;
    j["end_line"] = loc.endLine;
    j["end_column"] = loc.endColumn;
    j["kind"] = kg::db::locationKindToString(loc.locationKind);
    return j;
}

nlohmann::json JsonWriter::nodeObject(
    int64_t id,
    int kind,
    const std::string& serializedName,
    bool indexed,
    int definitionKind,
    const std::optional<kg::query::FileLocation>& location,
    std::optional<int> depth,
    bool seed) const
{
    const kg::db::DecodedName decoded = kg::db::decodeSerializedName(serializedName);

    nlohmann::json j;
    j["id"] = id;
    j["kind"] = kg::db::nodeKindToString(kind);
    j["name"] = decoded.elements.empty() ? std::string() : decoded.elements.back().name;
    j["qualified_name"] = decoded.namePath();
    j["signature"] = decoded.signature();

    const std::string qualified = decoded.qualifiedName();
    const std::string display = decoded.displayName();
    if (display != qualified)
    {
        // Only when Phase 3's hidden TU-local disambiguator suffix was
        // actually present -- see NameDecode.h's displayName()/
        // qualifiedName() doc and docs/JSON_SCHEMA.md.
        j["display_name"] = display;
    }

    j["indexed"] = indexed;
    j["definition_kind"] = indexed ? nlohmann::json(kg::db::definitionKindToString(definitionKind)) : nlohmann::json(nullptr);

    if (depth.has_value())
    {
        j["depth"] = *depth;
        j["seed"] = seed;
    }

    j["location"] = location.has_value() ? locationObject(*location) : nlohmann::json(nullptr);
    return j;
}

nlohmann::json JsonWriter::nodeObjectFromHydrated(int64_t id, const HydratedNode& n, std::optional<int> depth, bool seed) const
{
    return nodeObject(id, n.kind, n.serializedName, n.indexed, n.definitionKind, n.location, depth, seed);
}

nlohmann::json JsonWriter::candidateNodeObject(const kg::query::ResolvedSymbol& c) const
{
    const std::unordered_map<int64_t, SymbolMeta> meta = loadSymbolMeta({c.id});
    const std::unordered_map<int64_t, kg::query::FileLocation> loc = loadBestLocations({c.id});

    bool indexed = false;
    int definitionKind = 0;
    if (const auto it = meta.find(c.id); it != meta.end())
    {
        indexed = it->second.indexed;
        definitionKind = it->second.definitionKind;
    }
    std::optional<kg::query::FileLocation> location;
    if (const auto it = loc.find(c.id); it != loc.end())
    {
        location = it->second;
    }

    return nodeObject(c.id, c.kind, c.serializedName, indexed, definitionKind, location, std::nullopt, false);
}

std::vector<int64_t> JsonWriter::dedupSortedByKindName(const std::vector<int64_t>& idsIn, const std::unordered_map<int64_t, HydratedNode>& hydrated) const
{
    std::vector<int64_t> ids = dedupPreserveOrder(idsIn);
    std::sort(ids.begin(), ids.end(), [&](int64_t a, int64_t b) {
        const auto itA = hydrated.find(a);
        const auto itB = hydrated.find(b);
        const int kindA = itA != hydrated.end() ? itA->second.kind : 0;
        const int kindB = itB != hydrated.end() ? itB->second.kind : 0;
        if (kindA != kindB)
        {
            return kindA < kindB;
        }
        const std::string& nameA = itA != hydrated.end() ? itA->second.serializedName : std::string();
        const std::string& nameB = itB != hydrated.end() ? itB->second.serializedName : std::string();
        if (nameA != nameB)
        {
            return nameA < nameB;
        }
        return a < b;
    });
    return ids;
}

// --- kg.graph/1 --------------------------------------------------------------

nlohmann::json JsonWriter::graph(
    const QueryContext& ctx,
    const kg::query::BfsResult& result,
    int64_t elapsedMs,
    const std::unordered_map<std::string, nlohmann::json>* notesBySerializedName) const
{
    nlohmann::json j;
    j["kg_version"] = m_kgVersion;
    j["schema"] = "kg.graph/1";

    nlohmann::json q;
    q["verb"] = ctx.verb;
    q["db"] = ctx.db;
    q["symbol"] = ctx.symbolOrTextOrPath;
    q["symbol_ids"] = ctx.symbolIds;
    q["depth"] = ctx.depth;
    nlohmann::json edgeKinds = nlohmann::json::array();
    for (int k : ctx.edgeKinds)
    {
        edgeKinds.push_back(kg::db::edgeKindToString(k));
    }
    q["edge_kinds"] = edgeKinds;
    nlohmann::json nodeKinds = nlohmann::json::array();
    for (int k : ctx.nodeKinds)
    {
        nodeKinds.push_back(kg::db::nodeKindToString(k));
    }
    q["node_kinds"] = nodeKinds;
    q["max_nodes"] = ctx.maxNodes;
    j["query"] = std::move(q);

    j["truncated"] = result.truncated;

    int maxDepth = 0;
    for (const kg::query::BfsNode& n : result.nodes)
    {
        maxDepth = std::max(maxDepth, n.depth);
    }
    nlohmann::json stats;
    stats["node_count"] = result.nodes.size();
    stats["edge_count"] = result.edges.size();
    stats["levels"] = result.nodes.empty() ? 0 : maxDepth;
    stats["elapsed_ms"] = elapsedMs;
    j["stats"] = std::move(stats);

    std::vector<int64_t> nodeIds;
    nodeIds.reserve(result.nodes.size());
    for (const kg::query::BfsNode& n : result.nodes)
    {
        nodeIds.push_back(n.id);
    }
    const std::unordered_map<int64_t, HydratedNode> hydrated = hydrateNodes(nodeIds);
    const std::unordered_set<int64_t> seedSet(ctx.symbolIds.begin(), ctx.symbolIds.end());

    nlohmann::json nodesJson = nlohmann::json::array();
    for (const kg::query::BfsNode& n : result.nodes)
    {
        const auto it = hydrated.find(n.id);
        nlohmann::json nodeJson;
        std::string serializedNameForNotes = n.serializedName;
        if (it != hydrated.end())
        {
            nodeJson = nodeObjectFromHydrated(n.id, it->second, n.depth, seedSet.count(n.id) != 0);
            serializedNameForNotes = it->second.serializedName;
        }
        else
        {
            // Defensive fallback: the node table lookup somehow missed a row
            // the BFS itself just hydrated -- fall back to BFS's own
            // kind/serializedName rather than dropping the node from output.
            HydratedNode fallback;
            fallback.found = false;
            fallback.kind = n.kind;
            fallback.serializedName = n.serializedName;
            nodeJson = nodeObjectFromHydrated(n.id, fallback, n.depth, seedSet.count(n.id) != 0);
        }
        if (notesBySerializedName != nullptr)
        {
            const auto noteIt = notesBySerializedName->find(serializedNameForNotes);
            nodeJson["notes"] = noteIt != notesBySerializedName->end() ? noteIt->second : nlohmann::json(nullptr);
        }
        nodesJson.push_back(std::move(nodeJson));
    }
    j["nodes"] = std::move(nodesJson);

    std::vector<int64_t> edgeIds;
    edgeIds.reserve(result.edges.size());
    for (const kg::query::BfsEdge& e : result.edges)
    {
        edgeIds.push_back(e.id);
    }
    std::unordered_map<int64_t, std::vector<kg::query::FileLocation>> edgeLocs;
    if (!edgeIds.empty())
    {
        m_db.runChunkedIn(
            "SELECT o.element_id, l.file_node_id, l.start_line, l.start_column, l.end_line, l.end_column, l.type "
            "FROM occurrence o JOIN source_location l ON l.id = o.source_location_id "
            "WHERE o.element_id IN ({IN}) AND l.type IN (0,1,3,9)",
            dedupPreserveOrder(edgeIds),
            [&](sqlite3_stmt* stmt) {
                const int64_t id = sqlite3_column_int64(stmt, 0);
                kg::query::FileLocation loc;
                loc.fileId = sqlite3_column_int64(stmt, 1);
                loc.filePath = m_engine.fileMap().count(loc.fileId) != 0 ? m_engine.fileMap().at(loc.fileId) : std::string();
                loc.startLine = sqlite3_column_int(stmt, 2);
                loc.startColumn = sqlite3_column_int(stmt, 3);
                loc.endLine = sqlite3_column_int(stmt, 4);
                loc.endColumn = sqlite3_column_int(stmt, 5);
                loc.locationKind = sqlite3_column_int(stmt, 6);
                edgeLocs[id].push_back(std::move(loc));
            });
        for (auto& [id, locs] : edgeLocs)
        {
            std::sort(locs.begin(), locs.end(), [](const kg::query::FileLocation& a, const kg::query::FileLocation& b) {
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
    }

    nlohmann::json edgesJson = nlohmann::json::array();
    for (const kg::query::BfsEdge& e : result.edges)
    {
        nlohmann::json ej;
        ej["id"] = e.id;
        ej["kind"] = kg::db::edgeKindToString(e.kind);
        ej["source"] = e.source;
        ej["target"] = e.target;
        nlohmann::json locsJson = nlohmann::json::array();
        if (const auto it = edgeLocs.find(e.id); it != edgeLocs.end())
        {
            for (const kg::query::FileLocation& loc : it->second)
            {
                locsJson.push_back(locationObject(loc));
            }
        }
        ej["locations"] = std::move(locsJson);
        edgesJson.push_back(std::move(ej));
    }
    j["edges"] = std::move(edgesJson);

    return j;
}

// --- kg.references/1 ----------------------------------------------------------

nlohmann::json JsonWriter::references(const QueryContext& ctx, const kg::query::ReferencesResult& result, int64_t elapsedMs) const
{
    nlohmann::json j;
    j["kg_version"] = m_kgVersion;
    j["schema"] = "kg.references/1";

    nlohmann::json q;
    q["verb"] = ctx.verb;
    q["db"] = ctx.db;
    q["symbol"] = ctx.symbolOrTextOrPath;
    q["symbol_ids"] = ctx.symbolIds;
    j["query"] = std::move(q);

    j["truncated"] = false;  // references() has no cap/truncation concept

    nlohmann::json stats;
    stats["reference_count"] = result.hits.size();
    stats["elapsed_ms"] = elapsedMs;
    j["stats"] = std::move(stats);

    std::vector<int64_t> ids;
    for (const kg::query::ReferenceHit& h : result.hits)
    {
        ids.push_back(h.targetNodeId);
        ids.push_back(h.hasEdge ? h.contextNodeId : h.targetNodeId);
    }
    const std::unordered_map<int64_t, HydratedNode> hydrated = hydrateNodes(ids);
    const std::vector<int64_t> sortedIds = dedupSortedByKindName(ids, hydrated);

    nlohmann::json nodesJson = nlohmann::json::array();
    for (int64_t id : sortedIds)
    {
        const auto it = hydrated.find(id);
        nodesJson.push_back(nodeObjectFromHydrated(id, it != hydrated.end() ? it->second : HydratedNode{}, std::nullopt, false));
    }
    j["nodes"] = std::move(nodesJson);

    nlohmann::json refsJson = nlohmann::json::array();
    for (const kg::query::ReferenceHit& h : result.hits)
    {
        nlohmann::json rj;
        rj["edge_id"] = h.hasEdge ? nlohmann::json(h.edgeId) : nlohmann::json(nullptr);
        // "DECLARATION" is a synthetic label kg invents -- it is NOT a real
        // kg::db::EdgeKind from the schema, just a way to distinguish "this
        // location IS the symbol's own declaration" from "this location is a
        // reference edge pointing at it". See docs/JSON_SCHEMA.md.
        rj["edge_kind"] = h.hasEdge ? kg::db::edgeKindToString(h.edgeKind) : std::string("DECLARATION");
        rj["context"] = h.hasEdge ? h.contextNodeId : h.targetNodeId;
        rj["target"] = h.targetNodeId;
        rj["location"] = locationObject(h.location);
        refsJson.push_back(std::move(rj));
    }
    j["references"] = std::move(refsJson);

    return j;
}

// --- kg.definition/1 ----------------------------------------------------------

nlohmann::json JsonWriter::definition(const QueryContext& ctx, const kg::query::DefinitionResult& result, int64_t elapsedMs) const
{
    nlohmann::json j;
    j["kg_version"] = m_kgVersion;
    j["schema"] = "kg.definition/1";

    nlohmann::json q;
    q["verb"] = ctx.verb;
    q["db"] = ctx.db;
    q["symbol"] = ctx.symbolOrTextOrPath;
    q["symbol_ids"] = ctx.symbolIds;
    j["query"] = std::move(q);

    j["truncated"] = false;

    nlohmann::json stats;
    stats["location_count"] = result.locations.size();
    stats["elapsed_ms"] = elapsedMs;
    j["stats"] = std::move(stats);

    // Exactly one "symbol" object, not a nodes[] array (see JsonWriter.h).
    // With --all-matches (multiple seeds), only the FIRST seed is used here
    // -- result.locations itself is still the merged/sorted set across every
    // seed (an existing QueryEngine::definition() behavior, not new to
    // JSON); see docs/JSON_SCHEMA.md for this caveat.
    if (!ctx.symbolIds.empty())
    {
        const int64_t symId = ctx.symbolIds.front();
        const std::unordered_map<int64_t, HydratedNode> hydrated = hydrateNodes({symId});
        const auto it = hydrated.find(symId);
        j["symbol"] = nodeObjectFromHydrated(symId, it != hydrated.end() ? it->second : HydratedNode{}, std::nullopt, false);
    }
    else
    {
        j["symbol"] = nullptr;
    }

    nlohmann::json locsJson = nlohmann::json::array();
    for (const kg::query::FileLocation& loc : result.locations)
    {
        locsJson.push_back(locationObject(loc));
    }
    j["locations"] = std::move(locsJson);

    return j;
}

// --- kg.search/1 ---------------------------------------------------------------

nlohmann::json JsonWriter::search(const QueryContext& ctx, const std::vector<kg::query::SearchHit>& hits, int64_t elapsedMs) const
{
    nlohmann::json j;
    j["kg_version"] = m_kgVersion;
    j["schema"] = "kg.search/1";

    nlohmann::json q;
    q["verb"] = ctx.verb;
    q["db"] = ctx.db;
    q["text"] = ctx.symbolOrTextOrPath;
    q["limit"] = ctx.limit;
    nlohmann::json kinds = nlohmann::json::array();
    for (int k : ctx.searchKinds)
    {
        kinds.push_back(kg::db::nodeKindToString(k));
    }
    q["kinds"] = kinds;
    j["query"] = std::move(q);

    // search()'s `limit` is an intentional result cap, not a "truncation" in
    // the BFS max_nodes sense -- there's no reliable "were there more
    // matches" signal today without re-running unbounded, so this is always
    // false. See docs/JSON_SCHEMA.md.
    j["truncated"] = false;

    // Mirrors QueryEngine::search()'s own candidate-selection WHERE clause
    // exactly (see QueryEngine.cpp) so this count matches what that call
    // actually scanned before fuzzy-ranking.
    int64_t candidatesScanned = 0;
    if (ctx.searchKinds.empty())
    {
        m_db.run("SELECT COUNT(*) FROM node", [&](sqlite3_stmt* stmt) { candidatesScanned = sqlite3_column_int64(stmt, 0); });
    }
    else
    {
        int mask = 0;
        for (int k : ctx.searchKinds)
        {
            mask |= k;
        }
        m_db.run(
            "SELECT COUNT(*) FROM node WHERE (type & ?) != 0",
            [&](sqlite3_stmt* stmt) { sqlite3_bind_int(stmt, 1, mask); },
            [&](sqlite3_stmt* stmt) { candidatesScanned = sqlite3_column_int64(stmt, 0); });
    }

    nlohmann::json stats;
    stats["candidates_scanned"] = candidatesScanned;
    stats["hit_count"] = hits.size();
    stats["elapsed_ms"] = elapsedMs;
    j["stats"] = std::move(stats);

    std::vector<int64_t> ids;
    for (const kg::query::SearchHit& h : hits)
    {
        ids.push_back(h.nodeId);
    }
    const std::unordered_map<int64_t, HydratedNode> hydrated = hydrateNodes(ids);

    nlohmann::json hitsJson = nlohmann::json::array();
    for (const kg::query::SearchHit& h : hits)
    {
        nlohmann::json hj;
        hj["score"] = h.score;
        hj["matched_indices"] = h.matchedIndices;
        const auto it = hydrated.find(h.nodeId);
        hj["node"] = nodeObjectFromHydrated(h.nodeId, it != hydrated.end() ? it->second : HydratedNode{}, std::nullopt, false);
        hitsJson.push_back(std::move(hj));
    }
    j["hits"] = std::move(hitsJson);

    return j;
}

// --- kg.stats/1 ------------------------------------------------------------------

nlohmann::json JsonWriter::stats(const QueryContext& ctx, const kg::query::StatsResult& result) const
{
    nlohmann::json j;
    j["kg_version"] = m_kgVersion;
    j["schema"] = "kg.stats/1";

    nlohmann::json q;
    q["verb"] = ctx.verb;
    q["db"] = ctx.db;
    j["query"] = std::move(q);

    int64_t sizeBytes = 0;
    std::error_code ec;
    const auto fsize = std::filesystem::file_size(ctx.db, ec);
    if (!ec)
    {
        sizeBytes = static_cast<int64_t>(fsize);
    }

    nlohmann::json database;
    database["storage_version"] = result.storageVersion;
    database["path"] = ctx.db;
    database["size_bytes"] = sizeBytes;
    database["reader_indices_present"] = kg::db::missingIndexNames(m_db).empty();
    j["database"] = std::move(database);

    int64_t filesIndexed = 0;
    m_db.run("SELECT COUNT(*) FROM file WHERE indexed = 1", [&](sqlite3_stmt* stmt) { filesIndexed = sqlite3_column_int64(stmt, 0); });

    nlohmann::json counts;
    counts["files"] = result.fileCount;
    counts["files_indexed"] = filesIndexed;
    counts["nodes"] = result.nodeCount;
    counts["symbols"] = result.symbolCount;
    counts["edges"] = result.edgeCount;
    counts["source_locations"] = result.sourceLocationCount;
    counts["occurrences"] = result.occurrenceCount;
    counts["errors"] = result.errorCount;
    counts["local_symbols"] = result.localSymbolCount;  // additive: not in the worked example, kept for completeness
    j["counts"] = std::move(counts);

    // Sparse: only kinds with count > 0 (already true of nodesByKind/
    // edgesByKind -- see QueryEngine::stats()'s GROUP BY query).
    nlohmann::json nodeKinds = nlohmann::json::object();
    for (const kg::query::KindCount& kc : result.nodesByKind)
    {
        nodeKinds[kg::db::nodeKindToString(kc.kind)] = kc.count;
    }
    j["node_kinds"] = std::move(nodeKinds);

    nlohmann::json edgeKinds = nlohmann::json::object();
    for (const kg::query::KindCount& kc : result.edgesByKind)
    {
        edgeKinds[kg::db::edgeKindToString(kc.kind)] = kc.count;
    }
    j["edge_kinds"] = std::move(edgeKinds);

    nlohmann::json errors = nlohmann::json::array();
    for (const kg::query::StatsErrorRow& e : result.errors)
    {
        nlohmann::json ej;
        ej["id"] = e.id;
        ej["message"] = e.message;
        ej["fatal"] = e.fatal;
        ej["translation_unit"] = e.translationUnit;
        errors.push_back(std::move(ej));
    }
    j["errors"] = std::move(errors);

    return j;
}

// --- kg.file/1 -------------------------------------------------------------------

nlohmann::json JsonWriter::file(const QueryContext& ctx, const kg::query::FileResult& result, bool wantContents, bool wantOccurrences) const
{
    nlohmann::json j;
    j["kg_version"] = m_kgVersion;
    j["schema"] = "kg.file/1";

    nlohmann::json q;
    q["verb"] = ctx.verb;
    q["db"] = ctx.db;
    q["path"] = ctx.symbolOrTextOrPath;
    j["query"] = std::move(q);

    nlohmann::json fileObj;
    fileObj["id"] = result.fileId;
    fileObj["path"] = result.path;
    fileObj["language"] = result.language;
    fileObj["indexed"] = result.indexed;
    fileObj["complete"] = result.complete;
    fileObj["line_count"] = result.lineCount;
    j["file"] = std::move(fileObj);

    if (wantContents && result.hasContents)
    {
        j["contents"] = result.contents;
    }

    if (wantOccurrences)
    {
        // Real node-row occurrences get a resolved {kind, qualified_name}
        // (re-decoded from the actual node.serialized_name, not the
        // pre-decoded FileOccurrence::elementName -- which is displayName(),
        // a different string -- see JsonWriter.h). Zero-kind occurrences
        // (FileOccurrence::elementKind == 0 -- Phase 4's implicit "not a
        // node row" signal, see docs/KNOWN_LIMITATIONS.md) are resolved into
        // an EXPLICIT is_local_symbol boolean here instead.
        std::vector<int64_t> nodeElementIds;
        std::vector<int64_t> zeroKindElementIds;
        for (const kg::query::FileOccurrence& occ : result.occurrences)
        {
            if (occ.elementKind != 0)
            {
                nodeElementIds.push_back(occ.elementId);
            }
            else
            {
                zeroKindElementIds.push_back(occ.elementId);
            }
        }
        std::unordered_map<int64_t, HydratedNode> hydrated;
        m_db.runChunkedIn(
            "SELECT id, type, serialized_name FROM node WHERE id IN ({IN})", dedupPreserveOrder(nodeElementIds), [&](sqlite3_stmt* stmt) {
                HydratedNode node;
                node.found = true;
                node.kind = sqlite3_column_int(stmt, 1);
                node.serializedName = columnText(stmt, 2);
                hydrated[sqlite3_column_int64(stmt, 0)] = std::move(node);
            });
        const std::unordered_set<int64_t> localSymbolIds = loadLocalSymbolIds(zeroKindElementIds);

        nlohmann::json occArr = nlohmann::json::array();
        for (const kg::query::FileOccurrence& occ : result.occurrences)
        {
            nlohmann::json oj;
            oj["location"] = locationObject(occ.location);
            if (occ.elementKind != 0)
            {
                const auto it = hydrated.find(occ.elementId);
                nlohmann::json nodeObj;
                if (it != hydrated.end())
                {
                    const kg::db::DecodedName decoded = kg::db::decodeSerializedName(it->second.serializedName);
                    nodeObj["kind"] = kg::db::nodeKindToString(it->second.kind);
                    nodeObj["qualified_name"] = decoded.namePath();
                }
                else
                {
                    nodeObj["kind"] = kg::db::nodeKindToString(occ.elementKind);
                    nodeObj["qualified_name"] = occ.elementName;
                }
                oj["node"] = std::move(nodeObj);
                oj["is_local_symbol"] = false;
            }
            else
            {
                oj["is_local_symbol"] = localSymbolIds.count(occ.elementId) != 0;
            }
            occArr.push_back(std::move(oj));
        }
        j["occurrences"] = std::move(occArr);
    }

    return j;
}

// --- kg.ambiguous/1 / kg.not-found/1 -------------------------------------------

nlohmann::json JsonWriter::ambiguous(const QueryContext& ctx, const std::vector<kg::query::ResolvedSymbol>& candidates) const
{
    nlohmann::json j;
    j["kg_version"] = m_kgVersion;
    j["schema"] = "kg.ambiguous/1";

    nlohmann::json q;
    q["verb"] = ctx.verb;
    q["db"] = ctx.db;
    q["symbol"] = ctx.symbolOrTextOrPath;
    j["query"] = std::move(q);

    nlohmann::json arr = nlohmann::json::array();
    for (const kg::query::ResolvedSymbol& c : candidates)
    {
        arr.push_back(candidateNodeObject(c));
    }
    j["candidates"] = std::move(arr);

    return j;
}

nlohmann::json JsonWriter::notFound(const QueryContext& ctx, const std::vector<kg::query::SearchHit>& hits) const
{
    nlohmann::json j;
    j["kg_version"] = m_kgVersion;
    j["schema"] = "kg.not-found/1";

    nlohmann::json q;
    q["verb"] = ctx.verb;
    q["db"] = ctx.db;
    q["symbol"] = ctx.symbolOrTextOrPath;
    j["query"] = std::move(q);

    std::vector<int64_t> ids;
    for (const kg::query::SearchHit& h : hits)
    {
        ids.push_back(h.nodeId);
    }
    const std::unordered_map<int64_t, HydratedNode> hydrated = hydrateNodes(ids);

    nlohmann::json arr = nlohmann::json::array();
    for (const kg::query::SearchHit& h : hits)
    {
        const auto it = hydrated.find(h.nodeId);
        arr.push_back(nodeObjectFromHydrated(h.nodeId, it != hydrated.end() ? it->second : HydratedNode{}, std::nullopt, false));
    }
    j["suggestions"] = std::move(arr);

    return j;
}

nlohmann::json JsonWriter::notFoundFiles(const QueryContext& ctx, const std::vector<kg::query::FuzzyHit>& hits, const std::unordered_map<int64_t, std::string>& fileMap) const
{
    nlohmann::json j;
    j["kg_version"] = m_kgVersion;
    j["schema"] = "kg.not-found/1";

    nlohmann::json q;
    q["verb"] = ctx.verb;
    q["db"] = ctx.db;
    q["symbol"] = ctx.symbolOrTextOrPath;
    j["query"] = std::move(q);

    std::vector<int64_t> ids;
    for (const kg::query::FuzzyHit& h : hits)
    {
        ids.push_back(h.nodeId);
    }
    const std::unordered_map<int64_t, kg::query::FileLocation> locs = loadBestLocations(ids);

    nlohmann::json arr = nlohmann::json::array();
    for (const kg::query::FuzzyHit& h : hits)
    {
        const auto pathIt = fileMap.find(h.nodeId);
        const std::string path = pathIt != fileMap.end() ? pathIt->second : std::string();
        std::optional<kg::query::FileLocation> loc;
        if (const auto lit = locs.find(h.nodeId); lit != locs.end())
        {
            loc = lit->second;
        }
        arr.push_back(nodeObject(h.nodeId, kg::db::NodeKind::FILE, path, /*indexed=*/false, /*definitionKind=*/0, loc, std::nullopt, false));
    }
    j["suggestions"] = std::move(arr);

    return j;
}

}  // namespace kg::out::json
