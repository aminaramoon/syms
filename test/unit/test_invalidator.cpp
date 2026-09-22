// Exercises kg::update::{computeClearSet,invalidate} against a COPY of the
// real synthetic fixture database (build/fixture.srctrldb) -- see
// test/CMakeLists.txt's "fixture_query." registration (FIXTURES_REQUIRED
// FixtureDb) for how that database is produced. Each test case works
// against its own private temp copy so cases can't interfere with each
// other or with the pristine fixture other [fixture]-tagged tests rely on.

#include <catch2/catch_test_macros.hpp>

#include <filesystem>
#include <memory>
#include <unordered_set>

#include <sqlite3.h>

#include "kg/common/Paths.h"
#include "kg/db/Database.h"
#include "kg/db/WritableDb.h"
#include "kg/update/Invalidator.h"

using kg::db::Database;
using kg::db::WritableDb;
using kg::update::ClearPlan;
using kg::update::GcStats;

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

std::string sourceDir()
{
#ifdef KG_TEST_SOURCE_DIR
    return KG_TEST_SOURCE_DIR;
#else
    return ".";
#endif
}

// Copies the pristine fixture db to a private temp path (named after the
// calling test, via `tag`) so each TEST_CASE gets an isolated, mutable
// copy.
std::string copyFixture(const std::string& tag)
{
    const std::string path = (std::filesystem::temp_directory_path() / ("kg_test_invalidator_" + tag + ".srctrldb")).string();
    std::filesystem::remove(path);
    std::filesystem::copy_file(fixtureDbPath(), path);
    return path;
}

// Every kg_tu.path in `dbPath` -- the exact set of TU paths the fixture was
// indexed from, used as computeClearSet's `tuPaths` argument (see
// Invalidator.h: never prune a file that's still a TU itself).
std::vector<std::string> allTuPaths(const std::string& dbPath)
{
    Database db(dbPath);
    std::vector<std::string> paths;
    db.run("SELECT path FROM kg_tu", [&](sqlite3_stmt* stmt) {
        const char* p = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 0));
        if (p != nullptr)
        {
            paths.push_back(p);
        }
    });
    return paths;
}

// Resolves a fixture source path (relative to test/fixtures/synthetic) to
// its exact file.path spelling in `dbPath`, matched via normalizePath()
// (fixture.srctrldb is produced from a nested CMake configure, so its
// `file.path` values are whatever clang reports -- usually the same
// absolute path this test independently computes, but matched via
// normalization rather than assumed-identical string equality).
std::string resolveFixturePath(const std::string& dbPath, const std::string& relativeToSynthetic)
{
    const std::string wanted =
        kg::common::paths::normalize((std::filesystem::path(sourceDir()) / "fixtures" / "synthetic" / relativeToSynthetic).string());

    Database db(dbPath);
    std::string found;
    db.run("SELECT path FROM file", [&](sqlite3_stmt* stmt) {
        if (!found.empty())
        {
            return;
        }
        const char* p = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 0));
        if (p != nullptr && kg::common::paths::normalize(p) == wanted)
        {
            found = p;
        }
    });
    REQUIRE_FALSE(found.empty());
    return found;
}

std::unordered_set<std::string> baseNames(const std::vector<std::string>& paths)
{
    std::unordered_set<std::string> out;
    for (const auto& p : paths)
    {
        out.insert(std::filesystem::path(p).filename().string());
    }
    return out;
}

}  // namespace

TEST_CASE("invalidator: computeClearSet(chain.cpp) is exactly that one file (no header includes it)", "[fixture][invalidator]")
{
    const std::string path = copyFixture("chain_compute");
    const std::vector<std::string> tuPaths = allTuPaths(path);
    const std::string chainPath = resolveFixturePath(path, "src/chain.cpp");

    WritableDb db(path, true);
    const ClearPlan plan = kg::update::computeClearSet(db, {chainPath}, {}, tuPaths);

    REQUIRE(plan.clearedFileIds.size() == 1);
    REQUIRE(plan.clearedPaths.size() == 1);
    REQUIRE(plan.includeClosureAdded == 0);
    REQUIRE(plan.headersPruned == 0);
    REQUIRE(baseNames(plan.clearedPaths) == std::unordered_set<std::string>{"chain.cpp"});
}

TEST_CASE(
    "invalidator: computeClearSet(shapes.hpp) closes over exactly the TUs that include it (ns.cpp, overloads.cpp)",
    "[fixture][invalidator]")
{
    const std::string path = copyFixture("shapes_compute");
    const std::vector<std::string> tuPaths = allTuPaths(path);
    const std::string shapesPath = resolveFixturePath(path, "include/shapes.hpp");

    WritableDb db(path, true);
    const ClearPlan plan = kg::update::computeClearSet(db, {shapesPath}, {}, tuPaths);

    const std::unordered_set<std::string> names = baseNames(plan.clearedPaths);
    REQUIRE(names.count("shapes.hpp") == 1);
    REQUIRE(names.count("ns.cpp") == 1);
    REQUIRE(names.count("overloads.cpp") == 1);
    REQUIRE(plan.includeClosureAdded == 2);  // exactly the two TUs that include shapes.hpp

    // Header pruning (step 2) also correctly sweeps in macros.hpp and
    // <string> -- both are included ONLY by ns.cpp/overloads.cpp (now
    // cleared), so every one of their includers is cleared too. It must
    // NOT sweep in any of the four TUs that don't include shapes.hpp at
    // all (chain.cpp, templates.cpp, tu_a.cpp, tu_b.cpp).
    REQUIRE(names.count("chain.cpp") == 0);
    REQUIRE(names.count("templates.cpp") == 0);
    REQUIRE(names.count("tu_a.cpp") == 0);
    REQUIRE(names.count("tu_b.cpp") == 0);
}

TEST_CASE("invalidator: invalidate(chain.cpp plan) clears its facts, leaves untouched-file facts intact, and stays FK-consistent", "[fixture][invalidator]")
{
    const std::string path = copyFixture("chain_invalidate");
    const std::vector<std::string> tuPaths = allTuPaths(path);
    const std::string chainPath = resolveFixturePath(path, "src/chain.cpp");

    // `level0` is a chain.cpp-only symbol; `Shape` lives in shapes.hpp
    // (untouched by this plan) -- both looked up by serialized_name before
    // invalidation so post-invalidation existence can be checked by id.
    int64_t shapeNodeIdBefore = -1;
    {
        Database before(path);
        before.run("SELECT id FROM node WHERE serialized_name LIKE '%Shape%' AND serialized_name NOT LIKE '%Shapes%' LIMIT 1", [&](sqlite3_stmt* stmt) {
            shapeNodeIdBefore = sqlite3_column_int64(stmt, 0);
        });
    }
    REQUIRE(shapeNodeIdBefore >= 0);

    GcStats gc;
    ClearPlan plan;
    {
        WritableDb db(path, true);
        plan = kg::update::computeClearSet(db, {chainPath}, {}, tuPaths);
        REQUIRE(plan.clearedFileIds.size() == 1);
        gc = kg::update::invalidate(db, plan);
    }

    REQUIRE(gc.files == 1);

    Database after(path);

    // No source_location left pointing at the cleared file.
    int64_t remainingLocations = -1;
    after.run(
        "SELECT COUNT(*) FROM source_location l JOIN file f ON f.id = l.file_node_id WHERE f.path = ?1",
        [&](sqlite3_stmt* stmt) { sqlite3_bind_text(stmt, 1, chainPath.c_str(), -1, SQLITE_TRANSIENT); },
        [&](sqlite3_stmt* stmt) { remainingLocations = sqlite3_column_int64(stmt, 0); });
    REQUIRE(remainingLocations == 0);

    // No node with zero occurrences AND zero edges AND zero MEMBER
    // children, except a `file` row.
    int64_t deadNodes = -1;
    after.run(
        "SELECT COUNT(*) FROM node n WHERE n.id NOT IN (SELECT id FROM file) "
        "AND NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.element_id = n.id) "
        "AND NOT EXISTS (SELECT 1 FROM edge e WHERE e.type <> 1 AND (e.source_node_id = n.id OR e.target_node_id = n.id)) "
        "AND NOT EXISTS (SELECT 1 FROM edge e WHERE e.type = 1 AND e.source_node_id = n.id)",
        [&](sqlite3_stmt* stmt) { deadNodes = sqlite3_column_int64(stmt, 0); });
    REQUIRE(deadNodes == 0);

    // FK consistency (foreign_key_check runs regardless of PRAGMA
    // foreign_keys, and lists every violation row -- must be empty).
    int64_t fkViolations = 0;
    after.run("PRAGMA foreign_key_check", [&](sqlite3_stmt*) { ++fkViolations; });
    REQUIRE(fkViolations == 0);

    // A node defined only in an untouched file (shapes.hpp's Shape) must
    // still exist, by the SAME id.
    int64_t shapeNodeIdAfter = -1;
    after.run("SELECT id FROM node WHERE id = ?1", [&](sqlite3_stmt* stmt) { sqlite3_bind_int64(stmt, 1, shapeNodeIdBefore); }, [&](sqlite3_stmt* stmt) {
        shapeNodeIdAfter = sqlite3_column_int64(stmt, 0);
    });
    REQUIRE(shapeNodeIdAfter == shapeNodeIdBefore);
}

TEST_CASE(
    "invalidator: invalidate(shapes.hpp plan) leaves zero dangling kg_symbol_def/kg_tu rows",
    "[fixture][invalidator]")
{
    // shapes.hpp's closure clears ns.cpp/overloads.cpp too (via the
    // reverse-INCLUDE closure), which used to leave kg_symbol_def rows
    // pointing at now-dead node_id/file_node_id values behind (no FK on
    // that table) -- a landmine for id-reuse on the NEXT invalidate() run.
    const std::string path = copyFixture("shapes_dangling");
    const std::vector<std::string> tuPaths = allTuPaths(path);
    const std::string shapesPath = resolveFixturePath(path, "include/shapes.hpp");

    ClearPlan plan;
    {
        WritableDb db(path, true);
        plan = kg::update::computeClearSet(db, {shapesPath}, {}, tuPaths);
        REQUIRE(plan.clearedFileIds.size() > 1);
        kg::update::invalidate(db, plan);
    }

    Database after(path);

    int64_t danglingSymbolDef = -1;
    after.run(
        "SELECT COUNT(*) FROM kg_symbol_def WHERE node_id NOT IN (SELECT id FROM node) OR file_node_id NOT IN (SELECT id FROM file)",
        [&](sqlite3_stmt* stmt) { danglingSymbolDef = sqlite3_column_int64(stmt, 0); });
    REQUIRE(danglingSymbolDef == 0);

    int64_t danglingTu = -1;
    after.run(
        "SELECT COUNT(*) FROM kg_tu WHERE file_node_id IS NOT NULL AND file_node_id NOT IN (SELECT id FROM file)",
        [&](sqlite3_stmt* stmt) { danglingTu = sqlite3_column_int64(stmt, 0); });
    REQUIRE(danglingTu == 0);
}

TEST_CASE("invalidator: sweepOrphans on a pristine fixture copy is a no-op (nothing dead to begin with)", "[fixture][invalidator]")
{
    const std::string path = copyFixture("sweep_noop");
    WritableDb db(path, true);
    const GcStats gc = kg::update::sweepOrphans(db);
    REQUIRE(gc.nodes == 0);
    REQUIRE(gc.localSymbols == 0);
    REQUIRE(gc.errors == 0);
    REQUIRE(gc.nonIndexedFiles == 0);
    REQUIRE(gc.orphanElements == 0);
}
