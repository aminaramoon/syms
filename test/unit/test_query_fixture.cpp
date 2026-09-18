// Correctness tests for kg::query::QueryEngine (and a few kg::query::
// resolveSymbol / DB-conformance checks) against the real synthetic fixture
// database (build/fixture.srctrldb -- see test/CMakeLists.txt's
// FIXTURES_REQUIRED FixtureDb wiring for these [fixture]-tagged tests).

#include <catch2/catch_test_macros.hpp>

#include <algorithm>
#include <set>

#include <sqlite3.h>

#include "kg/db/Database.h"
#include "kg/db/Enums.h"
#include "kg/db/NameDecode.h"
#include "kg/query/Bfs.h"
#include "kg/query/QueryEngine.h"
#include "kg/query/SymbolResolver.h"

using kg::query::QueryEngine;
using kg::query::ResolveOutcome;

namespace
{

std::string fixtureDbPath()
{
#ifdef KG_FIXTURE_DB
    return KG_FIXTURE_DB;
#else
    return "fixture.srctrldb";
#endif
}

int64_t resolveOne(kg::db::Database& db, const std::string& spec, const std::vector<int>* kindFilter = nullptr)
{
    const kg::query::ResolveResult rr = kg::query::resolveSymbol(db, spec, kindFilter, nullptr);
    INFO("resolving '" << spec << "'");
    REQUIRE(rr.outcome == ResolveOutcome::Unique);
    return rr.candidates.front().id;
}

bool containsNode(const kg::query::BfsResult& r, int64_t id)
{
    return std::any_of(r.nodes.begin(), r.nodes.end(), [id](const kg::query::BfsNode& n) { return n.id == id; });
}

// bases()/derived()/etc. always include the seed(s) themselves at depth 0
// (see kg::query::Bfs.h -- BfsResult::nodes hydrates every visited node,
// seeds included); "X's bases" semantically excludes X itself, so tests
// that check "exactly {A, B, C}" compare against the non-seed nodes.
std::set<int64_t> nonSeedIds(const kg::query::BfsResult& r)
{
    std::set<int64_t> ids;
    for (const auto& n : r.nodes)
    {
        if (n.depth > 0)
        {
            ids.insert(n.id);
        }
    }
    return ids;
}

}  // namespace

TEST_CASE("query_fixture bases(Square) reaches exactly {Polygon,Colored,Shape} at depth 0 and {Polygon,Colored} at depth 1", "[fixture][query_fixture]")
{
    kg::db::Database db(fixtureDbPath());
    QueryEngine engine(db);

    const int64_t square = resolveOne(db, "fixture::Square");
    const int64_t polygon = resolveOne(db, "fixture::Polygon");
    const int64_t colored = resolveOne(db, "fixture::Colored");
    const int64_t shape = resolveOne(db, "fixture::Shape");

    const auto depth0 = nonSeedIds(engine.bases({square}, 0, 0));
    CHECK(depth0 == std::set<int64_t>{polygon, colored, shape});

    const auto depth1 = nonSeedIds(engine.bases({square}, 1, 0));
    CHECK(depth1 == std::set<int64_t>{polygon, colored});
}

TEST_CASE("query_fixture derived(Shape) reaches exactly {Polygon,Colored,Square} at depth 0", "[fixture][query_fixture]")
{
    kg::db::Database db(fixtureDbPath());
    QueryEngine engine(db);

    const int64_t shape = resolveOne(db, "fixture::Shape");
    const int64_t polygon = resolveOne(db, "fixture::Polygon");
    const int64_t colored = resolveOne(db, "fixture::Colored");
    const int64_t square = resolveOne(db, "fixture::Square");

    const auto depth0 = nonSeedIds(engine.derived({shape}, 0, 0));
    CHECK(depth0 == std::set<int64_t>{polygon, colored, square});
}

TEST_CASE("query_fixture OVERRIDE edges: Shape::area is transitively overridden by all 3 subclass methods", "[fixture][query_fixture]")
{
    // clang_getOverriddenCursors() only reports IMMEDIATE overrides (see
    // src/index/AstWalker.cpp's visitFunctionLikeDecl): Square::area only
    // directly overrides Polygon::area and Colored::area (both of which
    // directly override Shape::area), NOT Shape::area itself -- so only 2
    // OVERRIDE edges have target == Shape::area directly. Walking the full
    // OVERRIDE-edge subgraph reachable backward from Shape::area (unlimited
    // depth, not the QueryEngine::overriddenBy() convenience wrapper, which
    // is fixed at depth=1 per the verb's own contract) finds all 3
    // overriding methods and >= 3 edges, which is what this checks.
    kg::db::Database db(fixtureDbPath());

    const int64_t shapeArea = resolveOne(db, "fixture::Shape::area");
    const int64_t polygonArea = resolveOne(db, "fixture::Polygon::area");
    const int64_t coloredArea = resolveOne(db, "fixture::Colored::area");
    const int64_t squareArea = resolveOne(db, "fixture::Square::area");

    kg::query::BfsParams p;
    p.seeds = {shapeArea};
    p.edgeKindMask = kg::db::EdgeKind::OVERRIDE;
    p.forward = false;  // overriddenBy direction
    p.depth = 0;          // unlimited, unlike the depth=1 QueryEngine::overriddenBy() verb

    kg::query::BfsResult r = kg::query::runBfs(db, p);
    CHECK(r.edges.size() >= 3);
    CHECK(containsNode(r, polygonArea));
    CHECK(containsNode(r, coloredArea));
    CHECK(containsNode(r, squareArea));

    // And exactly 2 OVERRIDE edges have Shape::area as their DIRECT target.
    int directCount = 0;
    for (const auto& e : r.edges)
    {
        if (e.target == shapeArea)
        {
            ++directCount;
        }
    }
    CHECK(directCount == 2);

    // depth=1 QueryEngine::overriddenBy() only sees the direct overriders.
    QueryEngine engine(db);
    const auto direct = nonSeedIds(engine.overriddenBy({shapeArea}));
    CHECK(direct == std::set<int64_t>{polygonArea, coloredArea});
}

TEST_CASE("query_fixture callees(level0) is monotonically non-decreasing and saturates by depth 4", "[fixture][query_fixture]")
{
    kg::db::Database db(fixtureDbPath());
    QueryEngine engine(db);
    const int64_t level0 = resolveOne(db, "level0");

    std::size_t previous = 0;
    for (int depth = 1; depth <= 5; ++depth)
    {
        const auto ids = nonSeedIds(engine.callees({level0}, depth, 0));
        CHECK(ids.size() >= previous);
        previous = ids.size();
    }
    CHECK(previous == 4);  // level1, level2, level3, level4
}

TEST_CASE("query_fixture callers(pong) terminates on mutual recursion and includes ping", "[fixture][query_fixture]")
{
    kg::db::Database db(fixtureDbPath());
    QueryEngine engine(db);
    const int64_t pong = resolveOne(db, "pong");
    const int64_t ping = resolveOne(db, "ping");

    const kg::query::BfsResult r = engine.callers({pong}, 0, 0);
    CHECK_FALSE(r.truncated);
    CHECK(containsNode(r, pong));
    CHECK(containsNode(r, ping));
}

TEST_CASE("query_fixture header included by two TUs dedups to exactly one node per symbol", "[fixture][query_fixture]")
{
    kg::db::Database db(fixtureDbPath());
    // shapes.hpp is #included by both ns.cpp and overloads.cpp; if dedup by
    // USR-derived SymbolKey ever regressed, Square would show up as (at
    // least) 2 separate CLASS nodes with the exact same serialized_name.
    const std::string squareSerialized = "::\tmfixture\ts\tp\tnSquare\ts\tp";
    int64_t count = 0;
    db.run(
        "SELECT COUNT(*) FROM node WHERE type = ? AND serialized_name = ?",
        [&](sqlite3_stmt* stmt) {
            sqlite3_bind_int(stmt, 1, kg::db::NodeKind::CLASS);
            sqlite3_bind_text(stmt, 2, squareSerialized.c_str(), -1, SQLITE_TRANSIENT);
        },
        [&](sqlite3_stmt* stmt) { count = sqlite3_column_int64(stmt, 0); });
    CHECK(count == 1);
}

TEST_CASE("query_fixture resolveSymbol on the tu_a/tu_b USR-collision qualifiedNames resolves 4 distinct ids", "[fixture][query_fixture]")
{
    kg::db::Database db(fixtureDbPath());

    // Pulled from decodeSerializedName() of the real fixture rows (see
    // test_name_decode.cpp for how these strings were captured).
    const std::vector<std::string> specs = {
        "helper() [tu_a.cpp#317677a3]",
        "helper() [tu_b.cpp#c490d472]",
        "main() [tu_a.cpp#317677a3]",
        "main() [tu_b.cpp#c490d472]",
    };

    std::set<int64_t> ids;
    for (const std::string& spec : specs)
    {
        const kg::query::ResolveResult rr = kg::query::resolveSymbol(db, spec, nullptr, nullptr);
        INFO("resolving '" << spec << "'");
        REQUIRE(rr.outcome == ResolveOutcome::Unique);
        ids.insert(rr.candidates.front().id);
    }
    CHECK(ids.size() == 4);
}

TEST_CASE("query_fixture search('sqr', kind=CLASS) ranks Square first", "[fixture][query_fixture]")
{
    kg::db::Database db(fixtureDbPath());
    QueryEngine engine(db);

    const std::vector<int> classOnly{kg::db::NodeKind::CLASS};
    const auto hits = engine.search("sqr", &classOnly, 10);
    REQUIRE_FALSE(hits.empty());
    CHECK(hits.front().displayName == "fixture::Square");
}

TEST_CASE("query_fixture definition(Square) prefers the SCOPE location over TOKEN", "[fixture][query_fixture]")
{
    kg::db::Database db(fixtureDbPath());
    QueryEngine engine(db);
    const int64_t square = resolveOne(db, "fixture::Square");

    const auto def = engine.definition({square});
    REQUIRE_FALSE(def.locations.empty());
    for (const auto& loc : def.locations)
    {
        CHECK(loc.locationKind == kg::db::LocationKind::SCOPE);
    }
}

TEST_CASE("query_fixture references(Shape) locations are non-empty and point at real fixture source paths", "[fixture][query_fixture]")
{
    kg::db::Database db(fixtureDbPath());
    QueryEngine engine(db);
    const int64_t shape = resolveOne(db, "fixture::Shape");

    const auto refs = engine.references({shape});
    REQUIRE_FALSE(refs.hits.empty());
    for (const auto& hit : refs.hits)
    {
        CHECK_FALSE(hit.location.filePath.empty());
        CHECK(hit.location.filePath.find("shapes.hpp") != std::string::npos);
    }
}

TEST_CASE("query_fixture DB conformance: storage_version, all 13 tables, no orphan edges", "[fixture][query_fixture]")
{
    kg::db::Database db(fixtureDbPath());
    CHECK(db.storageVersion() == 25);

    static const std::vector<std::string> kExpectedTables = {
        "meta",
        "element",
        "element_component",
        "edge",
        "node",
        "symbol",
        "file",
        "filecontent",
        "local_symbol",
        "source_location",
        "occurrence",
        "component_access",
        "error",
    };

    std::set<std::string> present;
    db.run("SELECT name FROM sqlite_master WHERE type = 'table'", [&](sqlite3_stmt* stmt) {
        const unsigned char* text = sqlite3_column_text(stmt, 0);
        present.insert(text != nullptr ? reinterpret_cast<const char*>(text) : "");
    });

    for (const std::string& table : kExpectedTables)
    {
        INFO("expected table '" << table << "'");
        CHECK(present.count(table) == 1);
    }

    int64_t orphanCount = -1;
    db.run(
        "SELECT COUNT(*) FROM edge e "
        "LEFT JOIN node ns ON e.source_node_id = ns.id "
        "LEFT JOIN node nt ON e.target_node_id = nt.id "
        "WHERE ns.id IS NULL OR nt.id IS NULL",
        [&](sqlite3_stmt* stmt) { orphanCount = sqlite3_column_int64(stmt, 0); });
    CHECK(orphanCount == 0);
}
