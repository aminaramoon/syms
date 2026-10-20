// Feature 2 (`--parallel`/`-j`) equivalence tests: indexes the real
// synthetic fixture CDB (the same one test_json_shape.cpp/
// test_incremental_fixture.cpp use, via SYMHOW_FIXTURE_CDB) through
// symhow::index::runIndex() with different `parallel` values into separate temp
// databases, and asserts the RESULT is identical regardless of N -- both at
// the logical-snapshot level (symhow::update::diffSnapshots, id-independent)
// and, crucially, at the raw sqlite `node` table level INCLUDING ids: the
// design's determinism guarantee is that the reorder buffer makes the
// consumer see (and therefore assign ids to) TUs in the exact same CDB
// order no matter how many worker threads raced to produce them.

#include <catch2/catch_test_macros.hpp>

#include <string>
#include <vector>

#include <sqlite3.h>

#include "symhow/db/Database.h"
#include "symhow/index/IndexDriver.h"
#include "symhow/update/Snapshot.h"

using symhow::update::diffSnapshots;
using symhow::update::takeSnapshot;

namespace
{

std::string fixtureCdbPath()
{
#ifdef SYMHOW_FIXTURE_CDB
    return SYMHOW_FIXTURE_CDB;
#else
    return "fixture_build/compile_commands.json";
#endif
}

std::string testBinaryDir()
{
#ifdef SYMHOW_TEST_BINARY_DIR
    return SYMHOW_TEST_BINARY_DIR;
#else
    return ".";
#endif
}

// Runs a full `symhow index` (via the real runIndex(), never a shell-out) of
// the fixture CDB into `outputPath` with the given `parallel` value.
// Returns the process exit code (expected 0: the fixture always produces
// content).
int indexFixture(const std::string& outputPath, int parallel, int commitEvery = 0)
{
    symhow::index::IndexRunOptions opts;
    opts.compileCommandsPath = fixtureCdbPath();
    opts.outputPath = outputPath;
    opts.force = true;
    opts.quiet = true;
    opts.superQuiet = true;
    opts.parallel = parallel;
    opts.commitEvery = commitEvery;
    opts.symhowVersion = "test-version";
    return symhow::index::runIndex(opts);
}

// "id, serialized_name" pairs from `node`, ordered by id -- the strictest
// possible check: not just that the same logical nodes exist (that's what
// takeSnapshot/diffSnapshots already proves, id-independently) but that
// they were assigned the SAME ids in the SAME order.
std::vector<std::pair<int, std::string>> nodeIdsAndNames(const std::string& dbPath)
{
    symhow::db::Database db(dbPath);
    std::vector<std::pair<int, std::string>> rows;
    db.run("SELECT id, serialized_name FROM node ORDER BY id", [&](sqlite3_stmt* stmt) {
        const int id = sqlite3_column_int(stmt, 0);
        const char* name = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 1));
        rows.emplace_back(id, name != nullptr ? name : "");
    });
    return rows;
}

}  // namespace

TEST_CASE("parallel: parallel=1 vs parallel=4 produce an identical database (snapshot + node ids)", "[fixture][parallel]")
{
    const std::string dbA = testBinaryDir() + "/parallel_test_p1.srctrldb";
    const std::string dbB = testBinaryDir() + "/parallel_test_p4.srctrldb";

    REQUIRE(indexFixture(dbA, /*parallel=*/1) == 0);
    REQUIRE(indexFixture(dbB, /*parallel=*/4) == 0);

    const auto diff = diffSnapshots(takeSnapshot(dbA), takeSnapshot(dbB));
    CHECK(diff.onlyInA.empty());
    CHECK(diff.onlyInB.empty());

    // The determinism guarantee: node ids themselves (not just the logical
    // content) must match exactly, in the exact same order.
    const auto nodesA = nodeIdsAndNames(dbA);
    const auto nodesB = nodeIdsAndNames(dbB);
    REQUIRE_FALSE(nodesA.empty());
    REQUIRE(nodesA == nodesB);
}

TEST_CASE("parallel: parallel=0 (hardware_concurrency) also matches the sequential result", "[fixture][parallel]")
{
    const std::string dbA = testBinaryDir() + "/parallel_test_p1b.srctrldb";
    const std::string dbC = testBinaryDir() + "/parallel_test_p0.srctrldb";

    REQUIRE(indexFixture(dbA, /*parallel=*/1) == 0);
    REQUIRE(indexFixture(dbC, /*parallel=*/0) == 0);

    const auto diff = diffSnapshots(takeSnapshot(dbA), takeSnapshot(dbC));
    CHECK(diff.onlyInA.empty());
    CHECK(diff.onlyInB.empty());

    REQUIRE(nodeIdsAndNames(dbA) == nodeIdsAndNames(dbC));
}

TEST_CASE("parallel: parallel=4 with --commit-every 3 still matches the sequential, single-transaction result", "[fixture][parallel]")
{
    // Phase 8: checkpointing (commitEvery > 0) must not interact badly with
    // the parallel reorder buffer -- checkpoints only ever happen on the
    // single consumer thread, after a TU is consumed in strict CDB order,
    // so a chunk's symhow_tu rows are always a contiguous CDB prefix regardless
    // of how many worker threads raced to produce them.
    const std::string dbA = testBinaryDir() + "/parallel_test_p1_ce0.srctrldb";
    const std::string dbB = testBinaryDir() + "/parallel_test_p4_ce3.srctrldb";

    REQUIRE(indexFixture(dbA, /*parallel=*/1, /*commitEvery=*/0) == 0);
    REQUIRE(indexFixture(dbB, /*parallel=*/4, /*commitEvery=*/3) == 0);

    const auto diff = diffSnapshots(takeSnapshot(dbA), takeSnapshot(dbB));
    CHECK(diff.onlyInA.empty());
    CHECK(diff.onlyInB.empty());

    REQUIRE(nodeIdsAndNames(dbA) == nodeIdsAndNames(dbB));
}
