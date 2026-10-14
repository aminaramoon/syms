// Phase 8 (crash-safe, resumable indexing) tests against the real synthetic
// fixture CDB/DB -- see test/CMakeLists.txt's FixtureDb/FixtureCdb wiring
// (the same fixture test_incremental_fixture.cpp and test_parallel_fixture.cpp
// use).

#include <catch2/catch_test_macros.hpp>

#include <filesystem>
#include <string>
#include <vector>

#include <sqlite3.h>

#include "symhow/db/Database.h"
#include "symhow/db/Meta.h"
#include "symhow/index/IndexDriver.h"
#include "symhow/index/Provenance.h"
#include "symhow/update/Snapshot.h"
#include "symhow/update/UpdateDriver.h"

#ifdef SYMHOW_BINARY
#include "symhow/common/Process.h"
#endif

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

// The pre-built canonical fixture.srctrldb already carries real provenance
// (absolute cdbPath/repoRoot/repoCommit, discovered by the CLI's
// buildProvenance() at fixture-build time) -- reusing it (rather than
// synthesizing a fresh Provenance here) means every runIndex() call below
// indexes with EXACTLY the options/repo baseline fixture.srctrldb itself
// was built with, so it can be used as the "what a fresh, uninterrupted
// index produces" ground truth for dbdiff-style comparisons.
symhow::index::Provenance fixtureProvenance()
{
    symhow::db::Database db(fixtureDbPath());
    const auto prov = symhow::index::readProvenance(db.handle());
    REQUIRE(prov.has_value());
    return *prov;
}

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

int indexFixture(const std::string& outputPath, int commitEvery, long stopAfter = -1)
{
    const symhow::index::Provenance prov = fixtureProvenance();
    symhow::index::IndexRunOptions opts;
    opts.compileCommandsPath = fixtureCdbPath();
    opts.outputPath = outputPath;
    opts.force = true;
    opts.quiet = true;
    opts.superQuiet = true;
    opts.symhowVersion = "test-version";
    opts.commitEvery = commitEvery;
    opts.stopAfter = stopAfter;
    opts.dbOptions = prov.dbOptions;
    opts.sanitizerOptions = prov.sanitizerOptions;
    opts.systemFileRows = prov.systemFileRows;
    opts.provenance = prov;
    return symhow::index::runIndex(opts);
}

std::optional<std::string> readMeta(const std::string& dbPath, const std::string& key)
{
    symhow::db::Database db(dbPath);
    return symhow::db::getMeta(db.handle(), key);
}

}  // namespace

TEST_CASE("resume: --commit-every 1 vs default(25) vs 0 produce an identical database", "[fixture][resume]")
{
    const std::string dbA = testBinaryDir() + "/resume_test_ce1.srctrldb";
    const std::string dbB = testBinaryDir() + "/resume_test_ce25.srctrldb";
    const std::string dbC = testBinaryDir() + "/resume_test_ce0.srctrldb";

    REQUIRE(indexFixture(dbA, /*commitEvery=*/1) == 0);
    REQUIRE(indexFixture(dbB, /*commitEvery=*/25) == 0);
    REQUIRE(indexFixture(dbC, /*commitEvery=*/0) == 0);

    {
        const auto diff = diffSnapshots(takeSnapshot(dbA), takeSnapshot(dbB));
        CHECK(diff.onlyInA.empty());
        CHECK(diff.onlyInB.empty());
    }
    {
        const auto diff = diffSnapshots(takeSnapshot(dbA), takeSnapshot(dbC));
        CHECK(diff.onlyInA.empty());
        CHECK(diff.onlyInB.empty());
    }

    const auto nodesA = nodeIdsAndNames(dbA);
    REQUIRE_FALSE(nodesA.empty());
    // Checkpointing must not change node ids or their assignment order --
    // ties (equal-rank node.type upgrades) are decided identically whether
    // flushed once (commitEvery=0) or once per TU (commitEvery=1). See
    // SrctrldbBackend::checkpoint()'s doc comment.
    REQUIRE(nodesA == nodeIdsAndNames(dbB));
    REQUIRE(nodesA == nodeIdsAndNames(dbC));
}

TEST_CASE("resume: --stop-after interrupts cleanly and `symhow update` finishes it, matching a fresh index", "[fixture][resume]")
{
    const std::string dbPath = testBinaryDir() + "/resume_test_interrupted.srctrldb";
    std::filesystem::remove(dbPath);
    std::filesystem::remove(dbPath + "-journal");

    // The fixture CDB has 6 TUs (see test_incremental_fixture.cpp's own
    // comment) -- stop after 3, checkpointing every 2, so the interrupt
    // lands strictly after a checkpoint has already flushed once.
    const int rc = indexFixture(dbPath, /*commitEvery=*/2, /*stopAfter=*/3);
    REQUIRE(rc == 130);

    // No hot journal left -- the writer connection was closed cleanly.
    CHECK_FALSE(std::filesystem::exists(dbPath + "-journal"));

    // Openable read-only (verifies no corruption / no hanging lock).
    {
        symhow::db::Database db(dbPath);
        int tuCount = 0;
        db.run("SELECT count(*) FROM symhow_tu", [&](sqlite3_stmt* stmt) { tuCount = sqlite3_column_int(stmt, 0); });
        REQUIRE(tuCount == 3);
    }

    const auto inProgress = readMeta(dbPath, "symhow_index_in_progress");
    REQUIRE(inProgress.has_value());
    CHECK(*inProgress == "1");

#ifdef SYMHOW_BINARY
    // `symhow index` (no --force) on an in-progress db refuses with exit 2 and
    // points at `symhow update`/--force, rather than the generic "already
    // exists" exit 1.
    {
        const auto result = symhow::common::process::run({SYMHOW_BINARY, "index", "-c", fixtureCdbPath(), "-o", dbPath});
        CHECK(result.exitCode == 2);
        CHECK(result.err.find("symhow update") != std::string::npos);
    }

    // `symhow query stats` on the in-progress db still works (read-only) and
    // prints the interrupted-index notice on stderr.
    {
        const auto result = symhow::common::process::run({SYMHOW_BINARY, "query", "stats", "-d", dbPath});
        CHECK(result.exitCode == 0);
        CHECK(result.err.find("index incomplete") != std::string::npos);
    }
#endif

    // Resume via symhow::update::runUpdate() -- no --files, so this exercises
    // the real "no symhow_tu row at all" retry-set path (Phase 8 resume), not
    // just a git-diff-driven partial update.
    symhow::update::UpdateRunOptions updOpts;
    updOpts.dbPath = dbPath;
    updOpts.symhowVersion = "test-version";
    symhow::update::UpdateSummary summary;
    const int updRc = symhow::update::runUpdate(updOpts, &summary);
    REQUIRE(updRc == 0);
    // The never-indexed CDB ENTRIES: the fixture CDB has 8 entries for 7
    // distinct paths (templates.cpp is compiled twice -- see
    // fixtures/synthetic/CMakeLists.txt's fixture_lib_dup target); the
    // first 3 consumed by the interrupted run (ns.cpp, overloads.cpp,
    // chain.cpp) are neither of templates.cpp's two entries, so both of
    // its entries -- plus tu_a.cpp, tu_b.cpp, cfile.c -- are still pending:
    // 8 total - 3 already indexed = 5.
    CHECK(summary.tusToIndex == 5);

    const auto inProgressAfter = readMeta(dbPath, "symhow_index_in_progress");
    CHECK_FALSE(inProgressAfter.has_value());

    // The resumed-then-completed db must be logically identical to the
    // canonical fresh fixture.srctrldb -- same CDB, same options, same repo
    // baseline (see fixtureProvenance()'s comment).
    const auto diff = diffSnapshots(takeSnapshot(dbPath), takeSnapshot(fixtureDbPath()));
    CHECK(diff.onlyInA.empty());
    CHECK(diff.onlyInB.empty());

    symhow::db::Database finalDb(dbPath);
    int finalTuCount = 0;
    finalDb.run("SELECT count(*) FROM symhow_tu", [&](sqlite3_stmt* stmt) { finalTuCount = sqlite3_column_int(stmt, 0); });
    REQUIRE(finalTuCount == 7);
}

TEST_CASE(
    "resume: --stop-after landing strictly between a duplicated path's two entries leaves NO symhow_tu row for it, and "
    "`symhow update` still completes it exactly",
    "[fixture][resume]")
{
    // Regression test for the duplicate-CDB-entry + interrupted-index
    // fragility documented in docs/KNOWN_LIMITATIONS.md's "Phase 9"
    // section: `IndexDriver` only ever UPSERTs a path's `symhow_tu` row once
    // its LAST CDB entry (in CDB order) has been consumed -- never on an
    // earlier one -- so an interrupted run that stops strictly BETWEEN two
    // entries of the SAME duplicated path must leave that path with NO
    // `symhow_tu` row at all (not a row that wrongly claims it's done).
    //
    // The fixture CDB is, in CDB order (verified empirically and pinned by
    // the OTHER --stop-after test above, which relies on the first 3
    // entries being exactly ns.cpp/overloads.cpp/chain.cpp): ns.cpp,
    // overloads.cpp, chain.cpp, templates.cpp (fixture_lib's entry),
    // tu_a.cpp, tu_b.cpp, cfile.c, templates.cpp (fixture_lib_dup's entry,
    // see fixtures/synthetic/CMakeLists.txt) -- 8 entries, 7 distinct
    // paths. --stop-after 4 therefore consumes entries [0..3] -- through
    // templates.cpp's FIRST entry -- and stops strictly before its SECOND
    // entry (index 7) is ever reached: exactly the "interrupted between two
    // entries of a duplicated path" scenario.
    const std::string dbPath = testBinaryDir() + "/resume_test_dup_split.srctrldb";
    std::filesystem::remove(dbPath);
    std::filesystem::remove(dbPath + "-journal");

    const int rc = indexFixture(dbPath, /*commitEvery=*/2, /*stopAfter=*/4);
    REQUIRE(rc == 130);

    {
        symhow::db::Database db(dbPath);

        int tuCount = 0;
        db.run("SELECT count(*) FROM symhow_tu", [&](sqlite3_stmt* stmt) { tuCount = sqlite3_column_int(stmt, 0); });
        // ns.cpp, overloads.cpp, chain.cpp are each single-entry paths and
        // complete normally; templates.cpp's first entry was consumed but
        // -- the regression this pins -- must NOT have produced a row.
        REQUIRE(tuCount == 3);

        bool haveTemplatesRow = false;
        db.run("SELECT path FROM symhow_tu", [&](sqlite3_stmt* stmt) {
            const char* p = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 0));
            const std::string path = p != nullptr ? p : "";
            if (path.size() >= 13 && path.compare(path.size() - 13, 13, "templates.cpp") == 0)
            {
                haveTemplatesRow = true;
            }
        });
        REQUIRE_FALSE(haveTemplatesRow);
    }

    const auto inProgress = readMeta(dbPath, "symhow_index_in_progress");
    REQUIRE(inProgress.has_value());
    CHECK(*inProgress == "1");

    // Resume via symhow::update::runUpdate() -- no --files, so this exercises
    // the real "no symhow_tu row at all" retry-set path (Phase 8 resume).
    // Because templates.cpp has NO row at all, resume must pick up its
    // path and (via selectEntries(), which selects whole paths, not
    // individual entries) re-consume BOTH of its CDB entries -- including
    // re-processing the one already consumed (and checkpointed) before the
    // interrupt, which is harmless (SrctrldbBackend's node/symbol dedup is
    // idempotent for identical facts).
    symhow::update::UpdateRunOptions updOpts;
    updOpts.dbPath = dbPath;
    updOpts.symhowVersion = "test-version";
    symhow::update::UpdateSummary summary;
    const int updRc = symhow::update::runUpdate(updOpts, &summary);
    REQUIRE(updRc == 0);

    const auto inProgressAfter = readMeta(dbPath, "symhow_index_in_progress");
    CHECK_FALSE(inProgressAfter.has_value());

    // The resumed-then-completed db must be logically IDENTICAL to a fresh
    // full index -- the actual correctness bar this regression test exists
    // to enforce (a stale/missing symhow_tu row for the split path could
    // silently leave templates.cpp's second entry's facts out forever).
    const auto diff = diffSnapshots(takeSnapshot(dbPath), takeSnapshot(fixtureDbPath()));
    CHECK(diff.onlyInA.empty());
    CHECK(diff.onlyInB.empty());

    symhow::db::Database finalDb(dbPath);
    int finalTuCount = 0;
    finalDb.run("SELECT count(*) FROM symhow_tu", [&](sqlite3_stmt* stmt) { finalTuCount = sqlite3_column_int(stmt, 0); });
    REQUIRE(finalTuCount == 7);

    int templatesEntryCount = -1;
    finalDb.run("SELECT entry_count FROM symhow_tu WHERE path LIKE '%templates.cpp'", [&](sqlite3_stmt* stmt) {
        templatesEntryCount = sqlite3_column_int(stmt, 0);
    });
    REQUIRE(templatesEntryCount == 2);
}
