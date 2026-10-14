// Exercises symhow::query::runBfs() against the real synthetic fixture database
// (build/fixture.srctrldb, produced by the `fixture_cdb_generate` ->
// `fixture_index` CTest fixtures -- see test/CMakeLists.txt's
// FIXTURES_REQUIRED FixtureDb wiring for these [fixture]-tagged tests).

#include <catch2/catch_test_macros.hpp>

#include <algorithm>

#include "symhow/db/Database.h"
#include "symhow/db/Enums.h"
#include "symhow/query/Bfs.h"
#include "symhow/query/SymbolResolver.h"

using symhow::query::BfsParams;
using symhow::query::BfsResult;
using symhow::query::runBfs;

namespace
{

std::string fixtureDbPath()
{
#ifdef SYMHOW_FIXTURE_DB
    return SYMHOW_FIXTURE_DB;
#else
    return "fixture.srctrldb";
#endif
}

// Resolves `spec` (expected to be unambiguous -- callers should pass a
// fully-qualified or otherwise unique name) to a single node id, failing the
// current test loudly if it isn't.
int64_t resolveOne(symhow::db::Database& db, const std::string& spec)
{
    const symhow::query::ResolveResult rr = symhow::query::resolveSymbol(db, spec, nullptr, nullptr);
    INFO("resolving '" << spec << "'");
    REQUIRE(rr.outcome == symhow::query::ResolveOutcome::Unique);
    return rr.candidates.front().id;
}

bool containsNode(const BfsResult& r, int64_t id)
{
    return std::any_of(r.nodes.begin(), r.nodes.end(), [id](const symhow::query::BfsNode& n) { return n.id == id; });
}

std::size_t nonSeedNodeCount(const BfsResult& r)
{
    return static_cast<std::size_t>(std::count_if(r.nodes.begin(), r.nodes.end(), [](const symhow::query::BfsNode& n) { return n.depth > 0; }));
}

}  // namespace

TEST_CASE("bfs callees depth-limited traversal on chain.cpp's 5-level call chain saturates", "[fixture][bfs]")
{
    symhow::db::Database db(fixtureDbPath());
    const int64_t level0 = resolveOne(db, "level0");

    // level0 -> level1 -> level2 -> level3 -> level4 (4 hops, then no more).
    const std::size_t expectedNonSeedCounts[5] = {1, 2, 3, 4, 4};
    for (int depth = 1; depth <= 5; ++depth)
    {
        BfsParams p;
        p.seeds = {level0};
        p.edgeKindMask = symhow::db::EdgeKind::CALL;
        p.forward = true;
        p.depth = depth;
        BfsResult r = runBfs(db, p);
        INFO("depth=" << depth);
        CHECK(nonSeedNodeCount(r) == expectedNonSeedCounts[depth - 1]);
    }
}

TEST_CASE("bfs callees node counts are monotonically non-decreasing across increasing depth", "[fixture][bfs]")
{
    symhow::db::Database db(fixtureDbPath());
    const int64_t level0 = resolveOne(db, "level0");

    std::size_t previous = 0;
    for (int depth = 1; depth <= 5; ++depth)
    {
        BfsParams p;
        p.seeds = {level0};
        p.edgeKindMask = symhow::db::EdgeKind::CALL;
        p.forward = true;
        p.depth = depth;
        BfsResult r = runBfs(db, p);
        CHECK(nonSeedNodeCount(r) >= previous);
        previous = nonSeedNodeCount(r);
    }
}

TEST_CASE("bfs callers terminates on ping/pong mutual recursion and includes both", "[fixture][bfs]")
{
    symhow::db::Database db(fixtureDbPath());
    const int64_t pong = resolveOne(db, "pong");
    const int64_t ping = resolveOne(db, "ping");

    BfsParams p;
    p.seeds = {pong};
    p.edgeKindMask = symhow::db::EdgeKind::CALL;
    p.forward = false;  // callers: reverse
    p.depth = 0;         // unlimited -- must still terminate

    BfsResult r = runBfs(db, p);
    CHECK_FALSE(r.truncated);
    CHECK(containsNode(r, pong));
    CHECK(containsNode(r, ping));
}

TEST_CASE("bfs maxNodes caps the result and sets truncated", "[fixture][bfs]")
{
    symhow::db::Database db(fixtureDbPath());
    const int64_t square = resolveOne(db, "fixture::Square");

    BfsParams p;
    p.seeds = {square};
    p.edgeKindMask = symhow::db::EdgeKind::INHERITANCE;
    p.forward = true;  // bases direction
    p.depth = 0;        // would otherwise reach Polygon, Colored, AND Shape
    p.maxNodes = 2;      // seed + exactly 1 more

    BfsResult r = runBfs(db, p);
    CHECK(r.truncated);
    CHECK(r.nodes.size() == 2);
    // Both outgoing INHERITANCE edges from Square are still recorded even
    // though only one of their targets made it into the capped node list --
    // "keep the edge in the result either way".
    CHECK(r.edges.size() == 2);
}

TEST_CASE("bfs inheritance edge kind: bases direction reaches exactly Polygon, Colored, Shape", "[fixture][bfs]")
{
    symhow::db::Database db(fixtureDbPath());
    const int64_t square = resolveOne(db, "fixture::Square");
    const int64_t polygon = resolveOne(db, "fixture::Polygon");
    const int64_t colored = resolveOne(db, "fixture::Colored");
    const int64_t shape = resolveOne(db, "fixture::Shape");

    BfsParams p;
    p.seeds = {square};
    p.edgeKindMask = symhow::db::EdgeKind::INHERITANCE;
    p.forward = true;
    p.depth = 0;

    BfsResult r = runBfs(db, p);
    CHECK(nonSeedNodeCount(r) == 3);
    CHECK(containsNode(r, polygon));
    CHECK(containsNode(r, colored));
    CHECK(containsNode(r, shape));
}

TEST_CASE("bfs inheritance edge kind: derived direction reaches exactly Polygon, Colored, Square", "[fixture][bfs]")
{
    symhow::db::Database db(fixtureDbPath());
    const int64_t shape = resolveOne(db, "fixture::Shape");
    const int64_t polygon = resolveOne(db, "fixture::Polygon");
    const int64_t colored = resolveOne(db, "fixture::Colored");
    const int64_t square = resolveOne(db, "fixture::Square");

    BfsParams p;
    p.seeds = {shape};
    p.edgeKindMask = symhow::db::EdgeKind::INHERITANCE;
    p.forward = false;
    p.depth = 0;

    BfsResult r = runBfs(db, p);
    CHECK(nonSeedNodeCount(r) == 3);
    CHECK(containsNode(r, polygon));
    CHECK(containsNode(r, colored));
    CHECK(containsNode(r, square));
}

TEST_CASE("bfs depth=1 bases only reaches the immediate base classes, not Shape", "[fixture][bfs]")
{
    symhow::db::Database db(fixtureDbPath());
    const int64_t square = resolveOne(db, "fixture::Square");
    const int64_t polygon = resolveOne(db, "fixture::Polygon");
    const int64_t colored = resolveOne(db, "fixture::Colored");
    const int64_t shape = resolveOne(db, "fixture::Shape");

    BfsParams p;
    p.seeds = {square};
    p.edgeKindMask = symhow::db::EdgeKind::INHERITANCE;
    p.forward = true;
    p.depth = 1;

    BfsResult r = runBfs(db, p);
    CHECK(nonSeedNodeCount(r) == 2);
    CHECK(containsNode(r, polygon));
    CHECK(containsNode(r, colored));
    CHECK_FALSE(containsNode(r, shape));
}
