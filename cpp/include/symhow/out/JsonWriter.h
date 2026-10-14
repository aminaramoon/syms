#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <vector>

#include <nlohmann/json.hpp>

#include "symhow/db/Database.h"
#include "symhow/query/Bfs.h"
#include "symhow/query/FuzzyMatcher.h"
#include "symhow/query/QueryEngine.h"
#include "symhow/query/Results.h"
#include "symhow/query/SymbolResolver.h"

// Machine-readable (JSON) rendering of symhow::query::QueryEngine result types --
// the sibling of symhow::out::TextWriter (Phase 4). See docs/JSON_SCHEMA.md for
// the full envelope spec this implements (schema names, worked examples, and
// the two "invented, not literally from the DB schema" conventions:
// edge_kind "DECLARATION" in symhow.references/1, and is_local_symbol in
// symhow.file/1).
//
// Design note: TextWriter (Phase 4) is a set of PURE functions over
// already-computed Results.h/Bfs.h structs -- it needs nothing else. JSON
// output needs a little more per node than those structs carry (a single
// best "definition-ish" location, symbol.indexed/definition_kind, and --
// for the `file` verb's occurrences -- local_symbol table membership).
// Rather than growing Results.h/Bfs.h with fields only JSON output needs,
// JsonWriter hydrates that extra data itself via a handful of small batched
// queries against the same read-only symhow::db::Database its paired
// QueryEngine is already using (see hydrateNodes() in JsonWriter.cpp). The
// ONE exception is symhow::query::SearchHit::matchedIndices, added directly to
// Results.h -- see the comment there for why that one couldn't be hydrated
// after the fact (the fuzzy-match character positions aren't recoverable
// from a node id alone).
namespace symhow::out::json
{

// Everything about a `symhow query` invocation that a JSON envelope's "query"
// object needs, gathered in one place so each schema builder below can pull
// out just the fields relevant to it. Not every field is meaningful for
// every verb (e.g. `depth`/`maxNodes`/`edgeKinds` are graph-verb-only) --
// see JsonWriter.cpp for exactly which fields each schema builder reads.
struct QueryContext
{
    std::string verb;
    std::string db;
    std::string symbolOrTextOrPath;  // the raw positional argument (SYMBOL/TEXT/PATH), if any
    std::vector<int64_t> symbolIds;  // resolved BFS seed(s) / symbol id(s), if applicable

    // Graph verbs only (callers/callees/bases/derived/overrides/
    // overridden-by/members/neighbors).
    int depth = 0;
    int maxNodes = 0;
    std::vector<int> edgeKinds;  // symhow::db::EdgeKind values actually in play for this call
    std::vector<int> nodeKinds;  // symhow::db::NodeKind --kind filter, if any (empty = unfiltered)

    // `search` verb only.
    int limit = 0;
    std::vector<int> searchKinds;
};

class JsonWriter
{
public:
    // `db`/`engine` must outlive this JsonWriter (same lifetime contract as
    // QueryEngine itself holding a `Database&`). `symhowVersion` is the same
    // string `symhow --version` prints (SYMHOW_VERSION_STRING) -- passed in rather
    // than read from a macro here so this header has zero compile-time
    // coupling to how the CLI target is built.
    JsonWriter(symhow::db::Database& db, symhow::query::QueryEngine& engine, std::string symhowVersion);

    // symhow.graph/1 -- every graph-producing verb (callers callees bases
    // derived members neighbors overrides overridden-by). `elapsedMs` is the
    // caller-measured wall time of the QueryEngine call that produced
    // `result` (kept as an explicit parameter, not measured internally, so
    // callers -- including tests wanting byte-identical determinism -- fully
    // control it).
    // `notesBySerializedName`, if non-null, adds a "notes" field to every
    // node object: the map value (already the full symhow.note/1-shaped JSON
    // object, or JSON null) for that node's serialized_name if present,
    // else JSON null. When the pointer is null (every existing graph verb),
    // no "notes" field is added at all -- only `symhow query context` (Phase 4)
    // passes a non-null map, so symhow.graph/1's shape for callers/callees/etc.
    // is byte-for-byte unchanged. Kept as a plain
    // unordered_map<string, nlohmann::json> (not a symhow::notes::StoredNote
    // map) so symhow_out never has to depend on symhow_notes -- the CLI, which
    // links both, builds this map from the real StoredNote objects.
    nlohmann::json graph(
        const QueryContext& ctx,
        const symhow::query::BfsResult& result,
        int64_t elapsedMs,
        const std::unordered_map<std::string, nlohmann::json>* notesBySerializedName = nullptr) const;

    // symhow.references/1.
    nlohmann::json references(const QueryContext& ctx, const symhow::query::ReferencesResult& result, int64_t elapsedMs) const;

    // symhow.definition/1. Uses ctx.symbolIds.front() as the single "symbol"
    // object (see docs/JSON_SCHEMA.md for the --all-matches multi-seed
    // caveat this implies).
    nlohmann::json definition(const QueryContext& ctx, const symhow::query::DefinitionResult& result, int64_t elapsedMs) const;

    // symhow.search/1. stats.candidates_scanned (the total row count search()
    // considered before fuzzy-ranking) is recomputed here via a direct
    // COUNT(*) matching QueryEngine::search()'s own WHERE clause -- that
    // function doesn't return the count itself, and re-deriving it avoids
    // widening QueryEngine::search()'s signature just for this one JSON
    // field.
    nlohmann::json search(const QueryContext& ctx, const std::vector<symhow::query::SearchHit>& hits, int64_t elapsedMs) const;

    // symhow.stats/1. No elapsedMs parameter: StatsResult is itself a handful of
    // COUNT(*)s the CLI doesn't currently time as one unit (see
    // docs/JSON_SCHEMA.md).
    nlohmann::json stats(const QueryContext& ctx, const symhow::query::StatsResult& result) const;

    // symhow.file/1. `wantContents`/`wantOccurrences` mirror the CLI's
    // --contents/--occurrences flags (FileResult itself already conditions
    // result.hasContents/result.occurrences on them, but JsonWriter needs to
    // know whether to emit the "contents"/"occurrences" KEYS at all --
    // "optional" per the schema means absent-when-not-requested, not
    // present-with-an-empty-value).
    nlohmann::json file(const QueryContext& ctx, const symhow::query::FileResult& result, bool wantContents, bool wantOccurrences) const;

    // symhow.ambiguous/1 -- printed to stdout (in place of the text candidate
    // table) before exiting 2, for both symbol ambiguity
    // (resolveSymbol()/resolveSeedsOrReport()) and file-path ambiguity
    // (resolveFilePath()). Candidate node objects are built directly from
    // ResolvedSymbol's own {id, kind, serializedName} (matching exactly what
    // the existing text-mode candidate table already renders via
    // toCandidateRows()) rather than re-querying the `node` table by id --
    // deliberately, since a file-path candidate's ResolvedSymbol::
    // serializedName holds the file's `path`, not a real serialized_name
    // (see SymbolResolver.h's resolveFilePath() doc), and re-fetching the
    // real node row for that id could disagree with it.
    nlohmann::json ambiguous(const QueryContext& ctx, const std::vector<symhow::query::ResolvedSymbol>& candidates) const;

    // symhow.not-found/1 -- symbol case: `hits` from QueryEngine::search(spec,
    // nullptr, 5) exactly as the text-mode "did you mean" fallback already
    // computes.
    nlohmann::json notFound(const QueryContext& ctx, const std::vector<symhow::query::SearchHit>& hits) const;

    // symhow.not-found/1 -- file-path case: the CLI's `file` verb resolves
    // suggestions via symhow::query::fuzzyMatch() against the file map directly
    // (not QueryEngine::search(), which only ever ranks symbol nodes), so it
    // gets its own overload rather than forcing a SearchHit shape onto
    // FuzzyHit + a path string.
    nlohmann::json notFoundFiles(const QueryContext& ctx, const std::vector<symhow::query::FuzzyHit>& hits, const std::unordered_map<int64_t, std::string>& fileMap) const;

private:
    symhow::db::Database& m_db;
    symhow::query::QueryEngine& m_engine;
    std::string m_symhowVersion;

    // Per-node data JSON needs beyond what BfsNode/SearchHit/etc. carry.
    // `definitionKind == 0` means "no symbol row" (same convention as
    // symhow::db::DefinitionKind, which has no NONE value upstream).
    struct HydratedNode
    {
        bool found = false;
        int kind = 0;
        std::string serializedName;
        bool indexed = false;
        int definitionKind = 0;
        std::optional<symhow::query::FileLocation> location;
    };

    // Batched: one `node` query, one `symbol` query, one occurrence/
    // source_location query (SCOPE-preferred-over-TOKEN, mirroring
    // QueryEngine::definition()'s per-symbol logic -- see JsonWriter.cpp),
    // all chunked via Database::runChunkedIn. Duplicate ids in `ids` are
    // harmless (deduped internally); ids with no `node` row simply get a
    // default-constructed (found == false) entry.
    std::unordered_map<int64_t, HydratedNode> hydrateNodes(const std::vector<int64_t>& ids) const;

    // Just the location half of hydrateNodes(), factored out for callers
    // (ambiguous()/notFound()) that build the rest of the node object from a
    // ResolvedSymbol directly and only need the location lookup by id.
    std::unordered_map<int64_t, symhow::query::FileLocation> loadBestLocations(const std::vector<int64_t>& ids) const;

    // Just the symbol-table half: indexed + definition_kind by id.
    struct SymbolMeta
    {
        bool indexed = false;
        int definitionKind = 0;
    };
    std::unordered_map<int64_t, SymbolMeta> loadSymbolMeta(const std::vector<int64_t>& ids) const;

    // Which of `ids` are present in `local_symbol` (symhow.file/1's
    // is_local_symbol field).
    std::unordered_set<int64_t> loadLocalSymbolIds(const std::vector<int64_t>& ids) const;

    // The canonical node-object builder every schema funnels through. `name`
    // = the decoded name's bare leaf element (no ancestors, no signature);
    // `qualified_name` = DecodedName::namePath() (full "A::B::C" ancestor
    // path, no signature); `signature` = DecodedName::signature() (full
    // signature incl. return type). `display_name` is added only when
    // DecodedName::displayName() != DecodedName::qualifiedName() (i.e. only
    // when Phase 3's hidden TU-local disambiguator suffix is actually
    // present) -- see docs/JSON_SCHEMA.md. `depth`/`seed` are added only
    // when `depth` has a value (graph verbs only).
    nlohmann::json nodeObject(
        int64_t id,
        int kind,
        const std::string& serializedName,
        bool indexed,
        int definitionKind,
        const std::optional<symhow::query::FileLocation>& location,
        std::optional<int> depth,
        bool seed) const;

    nlohmann::json nodeObjectFromHydrated(int64_t id, const HydratedNode& n, std::optional<int> depth, bool seed) const;

    // ResolvedSymbol -> node object, per the ambiguous()/notFound() doc
    // comment above (no `node` table re-lookup).
    nlohmann::json candidateNodeObject(const symhow::query::ResolvedSymbol& c) const;

    static nlohmann::json locationObject(const symhow::query::FileLocation& loc);

    // Dedups and sorts `ids` by (kind, serializedName, id) -- the same
    // tie-break Phase 4's QueryEngine uses for its node lists (minus
    // `depth`, which doesn't apply to a plain id set) -- via `hydrated`.
    std::vector<int64_t> dedupSortedByKindName(const std::vector<int64_t>& ids, const std::unordered_map<int64_t, HydratedNode>& hydrated) const;
};

// Edge-kind bit values with a recorded occurrence location for at least one
// edge in a well-formed corpus (i.e. every named symhow::db::EdgeKind bit except
// UNKNOWN/ALL), used to decompose an arbitrary OR'd edge-kind mask (e.g.
// `neighbors`' default ALL) into the individual kind-name array the
// symhow.graph/1 "query.edge_kinds" field wants. Declared here (not just a
// JsonWriter.cpp implementation detail) since it's a reusable, self-
// contained utility -- symhow::db::Enums.h deliberately doesn't expose its
// internal {value,name} table, so this re-lists the same bit constants
// (symhow::db::EdgeKind::*, which ARE public) locally; keep in sync with
// src/db/Enums.cpp's kEdgeKindTable if a new edge kind is ever added.
std::vector<int> decomposeEdgeKindMask(int mask);

}  // namespace symhow::out::json
