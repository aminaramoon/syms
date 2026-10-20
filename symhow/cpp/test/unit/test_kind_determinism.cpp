// Regression test for the order-dependence bug fixed alongside this file:
// a symbol's final node.type used to be decided by "whichever TU/session
// observed a real (non-Unknown/Type) kind FIRST" whenever two observations
// tied on kindRank() (e.g. CLASS vs STRUCT, FUNCTION vs METHOD, GLOBAL_
// VARIABLE vs FIELD) -- see SrctrldbBackend::bufferKindDef()'s and
// flushPending()'s kUpdateNodeTypeSql comments, and docs/KNOWN_LIMITATIONS.md's
// Phase 8 section. Indexing the exact same CDB with its entries in a
// different order must not change node.type (or anything else) for any
// symbol: kindOrder(kind) = (kindRank(kind), nodeKindInt(kind)) makes the
// final kind a pure function of the SET of observations, not the order they
// arrived in.

#include <catch2/catch_test_macros.hpp>

#include <algorithm>
#include <string>
#include <vector>

#include <sqlite3.h>

#include "symhow/db/Database.h"
#include "symhow/index/CompileDb.h"
#include "symhow/index/IndexDriver.h"
#include "symhow/index/Provenance.h"
#include "symhow/update/Snapshot.h"

using symhow::update::diffSnapshots;
using symhow::update::takeSnapshot;

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

symhow::index::Provenance fixtureProvenance()
{
    symhow::db::Database db(fixtureDbPath());
    const auto prov = symhow::index::readProvenance(db.handle());
    REQUIRE(prov.has_value());
    return *prov;
}

// "serialized_name, type" pairs from `node`, ordered by serialized_name --
// deliberately id-independent (unlike test_resume_fixture.cpp/
// test_parallel_fixture.cpp's node-id checks): reversing CDB order also
// reverses the order nodes are first CREATED in, so ids themselves are
// expected to differ. What must NOT differ is which serialized_name maps
// to which node.type.
std::vector<std::pair<std::string, int>> namesAndTypes(const std::string& dbPath)
{
    symhow::db::Database db(dbPath);
    std::vector<std::pair<std::string, int>> rows;
    db.run("SELECT serialized_name, type FROM node ORDER BY serialized_name", [&](sqlite3_stmt* stmt) {
        const char* name = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 0));
        const int type = sqlite3_column_int(stmt, 1);
        rows.emplace_back(name != nullptr ? name : "", type);
    });
    return rows;
}

int indexEntries(const std::string& outputPath, const std::vector<symhow::index::CompileEntry>& entries)
{
    const symhow::index::Provenance prov = fixtureProvenance();
    symhow::index::IndexRunOptions opts;
    opts.compileCommandsPath = fixtureCdbPath();  // unused when presetEntries is set, kept for logging/consistency
    opts.outputPath = outputPath;
    opts.force = true;
    opts.quiet = true;
    opts.superQuiet = true;
    opts.symhowVersion = "test-version";
    opts.commitEvery = 0;
    opts.dbOptions = prov.dbOptions;
    opts.sanitizerOptions = prov.sanitizerOptions;
    opts.systemFileRows = prov.systemFileRows;
    opts.provenance = prov;
    opts.presetEntries = entries;
    return symhow::index::runIndex(opts);
}

}  // namespace

TEST_CASE("kind determinism: indexing the fixture CDB in reverse TU order produces an identical database", "[fixture]")
{
    const symhow::index::Provenance prov = fixtureProvenance();
    std::vector<symhow::index::CompileEntry> entries = symhow::index::load(fixtureCdbPath(), prov.dbOptions);
    REQUIRE_FALSE(entries.empty());

    std::vector<symhow::index::CompileEntry> reversed(entries.rbegin(), entries.rend());
    REQUIRE(reversed.size() == entries.size());

    const std::string dbForward = testBinaryDir() + "/kind_determinism_forward.srctrldb";
    const std::string dbReversed = testBinaryDir() + "/kind_determinism_reversed.srctrldb";

    REQUIRE(indexEntries(dbForward, entries) == 0);
    REQUIRE(indexEntries(dbReversed, reversed) == 0);

    // Logically identical (id-independent) ...
    const auto diff = diffSnapshots(takeSnapshot(dbForward), takeSnapshot(dbReversed));
    CHECK(diff.onlyInA.empty());
    CHECK(diff.onlyInB.empty());

    // ... AND, spelled out directly rather than via Snapshot's "name|type"
    // node key (diffSnapshots above already covers this, but a raw
    // `serialized_name -> type` table comparison is the most literal
    // possible statement of the property this fix guarantees): every
    // serialized_name maps to the exact same node.type in both databases,
    // regardless of which TU order happened to observe it first.
    const auto namesForward = namesAndTypes(dbForward);
    const auto namesReversed = namesAndTypes(dbReversed);
    REQUIRE_FALSE(namesForward.empty());
    REQUIRE(namesForward == namesReversed);
}
