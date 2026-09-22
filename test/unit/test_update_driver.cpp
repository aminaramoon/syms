// Exercises kg::update::runUpdate() (and its CLI wiring, `kg update`)
// against COPIES of the real synthetic fixture database -- see
// test/CMakeLists.txt's "fixture_query." registration (FIXTURES_REQUIRED
// FixtureDb) for how build/fixture.srctrldb and its compile_commands.json
// (KG_FIXTURE_CDB) are produced.

#include <catch2/catch_test_macros.hpp>

#include <filesystem>
#include <fstream>

#include <sqlite3.h>

#include "kg/db/Meta.h"
#include "kg/db/WritableDb.h"
#include "kg/update/Snapshot.h"
#include "kg/update/UpdateDriver.h"

#ifdef KG_BINARY
#include <nlohmann/json.hpp>

#include "kg/common/Process.h"
#endif

using kg::update::UpdateRunOptions;
using kg::update::UpdateSummary;
using kg::update::runUpdate;

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

std::string fixtureCdbPath()
{
#ifdef KG_FIXTURE_CDB
    return KG_FIXTURE_CDB;
#else
    return "compile_commands.json";
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

std::string copyFixture(const std::string& tag)
{
    const std::string path = (std::filesystem::temp_directory_path() / ("kg_test_update_driver_" + tag + ".srctrldb")).string();
    std::filesystem::remove(path);
    std::filesystem::copy_file(fixtureDbPath(), path);
    return path;
}

std::string synthPath(const std::string& relative)
{
    return std::filesystem::absolute(std::filesystem::path(sourceDir()) / "fixtures" / "synthetic" / relative).lexically_normal().string();
}

std::uintmax_t fileHash(const std::string& path)
{
    // Not a real hash -- just size + a checksum of the bytes, cheap and
    // sufficient to detect "did this file change at all" for the
    // byte-identical --dry-run assertion.
    std::ifstream f(path, std::ios::binary);
    REQUIRE(f.good());
    std::uintmax_t acc = 1469598103934665603ull;
    char buf[4096];
    while (f.read(buf, sizeof(buf)) || f.gcount() > 0)
    {
        for (std::streamsize i = 0; i < f.gcount(); ++i)
        {
            acc ^= static_cast<unsigned char>(buf[i]);
            acc *= 1099511628211ull;
        }
    }
    return acc;
}

int64_t nodeIdBySerializedNameLike(const std::string& dbPath, const std::string& pattern)
{
    kg::db::WritableDb db(dbPath, false);
    int64_t id = -1;
    db.query(
        "SELECT id FROM node WHERE serialized_name LIKE ?1 LIMIT 1",
        [&](sqlite3_stmt* stmt) { sqlite3_bind_text(stmt, 1, pattern.c_str(), -1, SQLITE_TRANSIENT); },
        [&](sqlite3_stmt* stmt) { id = sqlite3_column_int64(stmt, 0); });
    return id;
}

}  // namespace

TEST_CASE("update_driver: --files chain.cpp reproduces the fresh fixture db exactly (dbdiff empty)", "[fixture][update_driver]")
{
    const std::string path = copyFixture("chain_reproduce");
    const int64_t shapeIdBefore = nodeIdBySerializedNameLike(path, "%Shape%");
    REQUIRE(shapeIdBefore >= 0);

    UpdateRunOptions opts;
    opts.dbPath = path;
    opts.cdbPath = fixtureCdbPath();
    opts.explicitFiles = {synthPath("src/chain.cpp")};
    opts.kgVersion = "test";

    UpdateSummary summary;
    const int rc = runUpdate(opts, &summary);
    REQUIRE(rc == 0);

    const auto diff = kg::update::diffSnapshots(kg::update::takeSnapshot(path), kg::update::takeSnapshot(fixtureDbPath()));
    REQUIRE(diff.onlyInA.empty());
    REQUIRE(diff.onlyInB.empty());

    const int64_t shapeIdAfter = nodeIdBySerializedNameLike(path, "%Shape%");
    REQUIRE(shapeIdAfter == shapeIdBefore);
}

TEST_CASE("update_driver: a second no-op --files chain.cpp run retries nothing (Phase 4 kg_tu.status precision)", "[fixture][update_driver]")
{
    // Phase 4: kg_tu.status replaces the old (Phase 3) "no outgoing INCLUDE
    // edge" retry heuristic, which used to re-retry every include-free TU
    // (chain.cpp, templates.cpp, tu_a.cpp, tu_b.cpp in the fixture) on
    // EVERY run, forever. With precise per-TU status tracking, a second run
    // that only touches chain.cpp again must retry nothing and re-index
    // exactly the one requested TU.
    const std::string path = copyFixture("chain_second_run");

    UpdateRunOptions opts;
    opts.dbPath = path;
    opts.cdbPath = fixtureCdbPath();
    opts.explicitFiles = {synthPath("src/chain.cpp")};
    opts.kgVersion = "test";

    UpdateSummary first;
    REQUIRE(runUpdate(opts, &first) == 0);

    UpdateSummary second;
    REQUIRE(runUpdate(opts, &second) == 0);
    REQUIRE(second.tusRetried == 0);
    REQUIRE(second.tusToIndex == 1);

    const auto diff = kg::update::diffSnapshots(kg::update::takeSnapshot(path), kg::update::takeSnapshot(fixtureDbPath()));
    REQUIRE(diff.onlyInA.empty());
    REQUIRE(diff.onlyInB.empty());
}

TEST_CASE("update_driver: --files shapes.hpp (multi-TU closure) reproduces the fresh fixture db exactly", "[fixture][update_driver]")
{
    const std::string path = copyFixture("shapes_reproduce");

    UpdateRunOptions opts;
    opts.dbPath = path;
    opts.cdbPath = fixtureCdbPath();
    opts.explicitFiles = {synthPath("include/shapes.hpp")};
    opts.kgVersion = "test";

    UpdateSummary summary;
    const int rc = runUpdate(opts, &summary);
    REQUIRE(rc == 0);

    const auto diff = kg::update::diffSnapshots(kg::update::takeSnapshot(path), kg::update::takeSnapshot(fixtureDbPath()));
    REQUIRE(diff.onlyInA.empty());
    REQUIRE(diff.onlyInB.empty());
}

TEST_CASE("update_driver: --dry-run changes nothing and leaves no temp copy", "[fixture][update_driver]")
{
    const std::string path = copyFixture("dry_run");
    const std::uintmax_t before = fileHash(path);

    UpdateRunOptions opts;
    opts.dbPath = path;
    opts.cdbPath = fixtureCdbPath();
    opts.explicitFiles = {synthPath("src/chain.cpp")};
    opts.dryRun = true;
    opts.kgVersion = "test";

    UpdateSummary summary;
    const int rc = runUpdate(opts, &summary);
    REQUIRE(rc == 0);
    REQUIRE(summary.dryRun);

    REQUIRE(fileHash(path) == before);

    bool sawTempCopy = false;
    const std::filesystem::path dir = std::filesystem::path(path).parent_path();
    const std::string stem = std::filesystem::path(path).stem().string();
    for (const auto& entry : std::filesystem::directory_iterator(dir))
    {
        const std::string name = entry.path().filename().string();
        if (name.rfind(stem + ".incr-", 0) == 0)
        {
            sawTempCopy = true;
        }
    }
    REQUIRE_FALSE(sawTempCopy);
}

TEST_CASE(
    "update_driver: --in-place --dry-run leaves the DB byte-identical, and a later real update still succeeds",
    "[fixture][update_driver]")
{
    // Regression for the bug where --in-place --dry-run wrote
    // kg_update_in_progress=1 on the REAL file before the dry-run early
    // return, and nothing ever cleared it -- bricking every later `kg
    // update` (they'd all see the stale flag and refuse forever).
    const std::string path = copyFixture("inplace_dry_run");
    const std::uintmax_t before = fileHash(path);

    UpdateRunOptions opts;
    opts.dbPath = path;
    opts.cdbPath = fixtureCdbPath();
    opts.explicitFiles = {synthPath("src/chain.cpp")};
    opts.dryRun = true;
    opts.inPlace = true;
    opts.kgVersion = "test";

    UpdateSummary summary;
    const int rc = runUpdate(opts, &summary);
    REQUIRE(rc == 0);
    REQUIRE(summary.dryRun);
    REQUIRE(fileHash(path) == before);

    {
        kg::db::WritableDb db(path, false);
        const std::optional<std::string> inProgress = kg::db::getMeta(db.handle(), "kg_update_in_progress");
        REQUIRE_FALSE(inProgress.has_value());
    }

    // The real bug: a SUBSEQUENT real (non-dry-run) update must still
    // succeed -- it used to refuse with exit 2 forever after the dry run.
    UpdateRunOptions realOpts = opts;
    realOpts.dryRun = false;
    const int realRc = runUpdate(realOpts);
    REQUIRE(realRc == 0);
}

TEST_CASE("update_driver: --dry-run with nothing to do (exit 5) leaves the DB byte-identical", "[fixture][update_driver]")
{
    // Regression for the bug where the "nothing to do" branch refreshed
    // kg_repo_commit/kg_dirty_paths/kg_last_update_at even on --dry-run. A
    // --files path that's neither a known kg_tu row nor a CDB entry (here:
    // the fixture's own CMakeLists.txt) classifies as `unmatched`, which
    // produces an empty clearSeed/deletedSeed/newTus -- i.e. genuinely
    // "nothing to do" (exit 5) -- so this exercises that branch directly,
    // unlike the plain --dry-run test above (which clears a real file and
    // returns exit 0).
    const std::string path = copyFixture("dry_run_nothing_to_do");
    const std::uintmax_t before = fileHash(path);

    UpdateRunOptions opts;
    opts.dbPath = path;
    opts.cdbPath = fixtureCdbPath();
    opts.explicitFiles = {synthPath("CMakeLists.txt")};
    opts.dryRun = true;
    opts.kgVersion = "test";

    UpdateSummary summary;
    const int rc = runUpdate(opts, &summary);
    REQUIRE(rc == 5);
    REQUIRE(summary.nothingToDo);
    REQUIRE(fileHash(path) == before);
}

TEST_CASE("update_driver: a stale lock file makes runUpdate refuse with exit 2", "[fixture][update_driver]")
{
    const std::string path = copyFixture("lock_refuse");
    const std::string lockPath = path + ".kg-lock";
    std::filesystem::remove(lockPath);
    {
        std::ofstream lockFile(lockPath);
        lockFile << "held by a fake pid\n";
    }

    UpdateRunOptions opts;
    opts.dbPath = path;
    opts.cdbPath = fixtureCdbPath();
    opts.explicitFiles = {synthPath("src/chain.cpp")};
    opts.kgVersion = "test";

    const int rc = runUpdate(opts);
    REQUIRE(rc == 2);

    // The lock file must still be there (runUpdate must never remove a
    // lock it didn't itself create).
    REQUIRE(std::filesystem::exists(lockPath));
    std::filesystem::remove(lockPath);

    // With the stale lock gone, the same request now succeeds.
    const int rc2 = runUpdate(opts);
    REQUIRE(rc2 == 0);
    REQUIRE_FALSE(std::filesystem::exists(lockPath));
}

TEST_CASE("update_driver: refuses a database with no provenance", "[fixture][update_driver]")
{
    const std::string path = copyFixture("no_provenance");
    {
        kg::db::WritableDb db(path, false);
        db.begin();
        kg::db::deleteMeta(db, "kg_provenance_version");
        db.commit();
    }

    UpdateRunOptions opts;
    opts.dbPath = path;
    opts.cdbPath = fixtureCdbPath();
    opts.explicitFiles = {synthPath("src/chain.cpp")};
    opts.kgVersion = "test";

    const int rc = runUpdate(opts);
    REQUIRE(rc == 2);
}

TEST_CASE("update_driver: refuses a database with kg_update_in_progress set", "[fixture][update_driver]")
{
    const std::string path = copyFixture("in_progress");
    {
        kg::db::WritableDb db(path, false);
        db.begin();
        kg::db::setMeta(db, "kg_update_in_progress", "1");
        db.commit();
    }

    UpdateRunOptions opts;
    opts.dbPath = path;
    opts.cdbPath = fixtureCdbPath();
    opts.explicitFiles = {synthPath("src/chain.cpp")};
    opts.kgVersion = "test";

    const int rc = runUpdate(opts);
    REQUIRE(rc == 2);
}

#ifdef KG_BINARY
TEST_CASE("update_driver: `kg update --json` CLI smoke emits kg.update-summary/1", "[fixture][update_driver]")
{
    const std::string path = copyFixture("cli_smoke");

    const auto result = kg::common::process::run(
        {KG_BINARY, "update", "-d", path, "-c", fixtureCdbPath(), "--files", synthPath("src/chain.cpp"), "--json"});
    REQUIRE(result.exitCode == 0);

    const nlohmann::json j = nlohmann::json::parse(result.out);
    REQUIRE(j.at("schema").get<std::string>() == "kg.update-summary/1");
    REQUIRE(j.contains("files"));
    REQUIRE(j.at("files").contains("cleared_paths"));
    REQUIRE(j.contains("gc"));
    REQUIRE(j.contains("tus"));
}
#endif
