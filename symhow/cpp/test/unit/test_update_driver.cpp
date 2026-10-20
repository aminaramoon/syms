// Exercises symhow::update::runUpdate() (and its CLI wiring, `symhow update`)
// against COPIES of the real synthetic fixture database -- see
// test/CMakeLists.txt's "fixture_query." registration (FIXTURES_REQUIRED
// FixtureDb) for how build/fixture.srctrldb and its compile_commands.json
// (SYMHOW_FIXTURE_CDB) are produced.

#include <catch2/catch_test_macros.hpp>

#include <filesystem>
#include <fstream>
#include <string>
#include <utility>

#include <sqlite3.h>

#include "symhow/db/Meta.h"
#include "symhow/db/WritableDb.h"
#include "symhow/update/Snapshot.h"
#include "symhow/update/UpdateDriver.h"

#ifdef SYMHOW_BINARY
#include <nlohmann/json.hpp>

#include "symhow/common/Process.h"
#endif

using symhow::update::UpdateRunOptions;
using symhow::update::UpdateSummary;
using symhow::update::runUpdate;

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
    return "compile_commands.json";
#endif
}

std::string sourceDir()
{
#ifdef SYMHOW_TEST_SOURCE_DIR
    return SYMHOW_TEST_SOURCE_DIR;
#else
    return ".";
#endif
}

std::string copyFixture(const std::string& tag)
{
    const std::string path = (std::filesystem::temp_directory_path() / ("symhow_test_update_driver_" + tag + ".srctrldb")).string();
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
    symhow::db::WritableDb db(dbPath, false);
    int64_t id = -1;
    db.query(
        "SELECT id FROM node WHERE serialized_name LIKE ?1 LIMIT 1",
        [&](sqlite3_stmt* stmt) { sqlite3_bind_text(stmt, 1, pattern.c_str(), -1, SQLITE_TRANSIENT); },
        [&](sqlite3_stmt* stmt) { id = sqlite3_column_int64(stmt, 0); });
    return id;
}

// (args_hash, entry_count) of the symhow_tu row whose path ends with `suffix`
// (e.g. "templates.cpp") -- used to prove a duplicated path's row is left
// completely untouched by an update that never re-indexes it.
std::pair<std::string, int64_t> symhowTuArgsHashAndCount(const std::string& dbPath, const std::string& suffix)
{
    symhow::db::WritableDb db(dbPath, false);
    std::pair<std::string, int64_t> result;
    bool found = false;
    db.query(
        "SELECT path, args_hash, entry_count FROM symhow_tu",
        [&](sqlite3_stmt* stmt) {
            const char* p = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 0));
            const std::string path = p != nullptr ? p : "";
            if (path.size() >= suffix.size() && path.compare(path.size() - suffix.size(), suffix.size(), suffix) == 0)
            {
                const char* h = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 1));
                result.first = h != nullptr ? h : "";
                result.second = sqlite3_column_int64(stmt, 2);
                found = true;
            }
        });
    REQUIRE(found);
    return result;
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
    opts.symhowVersion = "test";

    UpdateSummary summary;
    const int rc = runUpdate(opts, &summary);
    REQUIRE(rc == 0);

    const auto diff = symhow::update::diffSnapshots(symhow::update::takeSnapshot(path), symhow::update::takeSnapshot(fixtureDbPath()));
    REQUIRE(diff.onlyInA.empty());
    REQUIRE(diff.onlyInB.empty());

    const int64_t shapeIdAfter = nodeIdBySerializedNameLike(path, "%Shape%");
    REQUIRE(shapeIdAfter == shapeIdBefore);
}

TEST_CASE("update_driver: a second no-op --files chain.cpp run retries nothing (Phase 4 symhow_tu.status precision)", "[fixture][update_driver]")
{
    // Phase 4: symhow_tu.status replaces the old (Phase 3) "no outgoing INCLUDE
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
    opts.symhowVersion = "test";

    UpdateSummary first;
    REQUIRE(runUpdate(opts, &first) == 0);

    UpdateSummary second;
    REQUIRE(runUpdate(opts, &second) == 0);
    REQUIRE(second.tusRetried == 0);
    REQUIRE(second.tusToIndex == 1);

    const auto diff = symhow::update::diffSnapshots(symhow::update::takeSnapshot(path), symhow::update::takeSnapshot(fixtureDbPath()));
    REQUIRE(diff.onlyInA.empty());
    REQUIRE(diff.onlyInB.empty());
}

TEST_CASE(
    "update_driver: duplicate-CDB-entry regression -- updating an UNRELATED file never false-flags a duplicated "
    "path, and a duplicated path's symhow_tu row covers ALL its entries",
    "[fixture][update_driver]")
{
    // The bug this fixes: symhow_tu is keyed by PATH, but the fixture's CDB has
    // TWO entries for templates.cpp (see fixtures/synthetic/CMakeLists.txt's
    // fixture_lib_dup target, SYMHOW_DUP=1) -- one file, two different sets of
    // compiler flags. Before the fix, symhow_tu.args_hash was whichever entry's
    // computeArgsHash() happened to be UPSERTed LAST during indexing, so
    // the OTHER entry's hash would never match on the next `symhow update` --
    // "flag changed" -- and templates.cpp's TWO entries would be
    // re-indexed on EVERY SINGLE run, even one that only touches a
    // completely unrelated file (chain.cpp here). With the fix (a single
    // combined, order-independent hash per path -- symhow::index::
    // pathArgsHashes()), templates.cpp must never show up as flag-changed
    // just because chain.cpp changed.
    const std::string path = copyFixture("dup_entry_unrelated_update");

    const auto templatesRowBefore = symhowTuArgsHashAndCount(path, "templates.cpp");
    REQUIRE(templatesRowBefore.second == 2);  // entry_count -- see fixture_lib_dup

    UpdateRunOptions opts;
    opts.dbPath = path;
    opts.cdbPath = fixtureCdbPath();
    opts.explicitFiles = {synthPath("src/chain.cpp")};  // unrelated to templates.cpp's duplicate entries
    opts.symhowVersion = "test";

    UpdateSummary first;
    REQUIRE(runUpdate(opts, &first) == 0);
    // The regression itself: no path (in particular not templates.cpp) is
    // reported as flag-changed by an update that never touched it.
    REQUIRE(first.flagChangedCount == 0);
    // Only chain.cpp's own single CDB entry is re-indexed -- not
    // templates.cpp's two entries, which the pre-fix hash thrashing would
    // have dragged in as a false "flag changed".
    REQUIRE(first.tusToIndex == 1);
    // templates.cpp's symhow_tu row (combined hash + entry_count) is untouched.
    REQUIRE(symhowTuArgsHashAndCount(path, "templates.cpp") == templatesRowBefore);

    // --files always forces its own target to be treated as Modified
    // (that's what --files is FOR), so a second identical --files chain.cpp
    // run legitimately re-indexes chain.cpp again too -- --files bypasses
    // the flag-change machinery for its OWN targets entirely. What must NOT
    // happen, on this or any other run, is templates.cpp's path getting
    // dragged in as a false "flag changed": before the fix, symhow_tu.args_hash
    // was whichever of templates.cpp's two entries happened to be UPSERTed
    // last, so the OTHER entry's computeArgsHash() would never match on the
    // NEXT run -- re-indexing templates.cpp's two entries on EVERY update,
    // forever, regardless of --files.
    UpdateSummary second;
    REQUIRE(runUpdate(opts, &second) == 0);
    REQUIRE(second.flagChangedCount == 0);
    REQUIRE(second.tusToIndex == 1);
    REQUIRE(symhowTuArgsHashAndCount(path, "templates.cpp") == templatesRowBefore);

    // The resulting db must still be logically identical to the canonical
    // fresh fixture (templates.cpp's symhow_tu row -- combined hash,
    // entry_count=2 -- was never touched by either update, and correctly
    // still describes both of its CDB entries).
    const auto diff = symhow::update::diffSnapshots(symhow::update::takeSnapshot(path), symhow::update::takeSnapshot(fixtureDbPath()));
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
    opts.symhowVersion = "test";

    UpdateSummary summary;
    const int rc = runUpdate(opts, &summary);
    REQUIRE(rc == 0);

    const auto diff = symhow::update::diffSnapshots(symhow::update::takeSnapshot(path), symhow::update::takeSnapshot(fixtureDbPath()));
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
    opts.symhowVersion = "test";

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
    // symhow_update_in_progress=1 on the REAL file before the dry-run early
    // return, and nothing ever cleared it -- bricking every later `symhow
    // update` (they'd all see the stale flag and refuse forever).
    const std::string path = copyFixture("inplace_dry_run");
    const std::uintmax_t before = fileHash(path);

    UpdateRunOptions opts;
    opts.dbPath = path;
    opts.cdbPath = fixtureCdbPath();
    opts.explicitFiles = {synthPath("src/chain.cpp")};
    opts.dryRun = true;
    opts.inPlace = true;
    opts.symhowVersion = "test";

    UpdateSummary summary;
    const int rc = runUpdate(opts, &summary);
    REQUIRE(rc == 0);
    REQUIRE(summary.dryRun);
    REQUIRE(fileHash(path) == before);

    {
        symhow::db::WritableDb db(path, false);
        const std::optional<std::string> inProgress = symhow::db::getMeta(db.handle(), "symhow_update_in_progress");
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
    // symhow_repo_commit/symhow_dirty_paths/symhow_last_update_at even on --dry-run. A
    // --files path that's neither a known symhow_tu row nor a CDB entry (here:
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
    opts.symhowVersion = "test";

    UpdateSummary summary;
    const int rc = runUpdate(opts, &summary);
    REQUIRE(rc == 5);
    REQUIRE(summary.nothingToDo);
    REQUIRE(fileHash(path) == before);
}

TEST_CASE("update_driver: a stale lock file makes runUpdate refuse with exit 2", "[fixture][update_driver]")
{
    const std::string path = copyFixture("lock_refuse");
    const std::string lockPath = path + ".symhow-lock";
    std::filesystem::remove(lockPath);
    {
        std::ofstream lockFile(lockPath);
        lockFile << "held by a fake pid\n";
    }

    UpdateRunOptions opts;
    opts.dbPath = path;
    opts.cdbPath = fixtureCdbPath();
    opts.explicitFiles = {synthPath("src/chain.cpp")};
    opts.symhowVersion = "test";

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
        symhow::db::WritableDb db(path, false);
        db.begin();
        symhow::db::deleteMeta(db, "symhow_provenance_version");
        db.commit();
    }

    UpdateRunOptions opts;
    opts.dbPath = path;
    opts.cdbPath = fixtureCdbPath();
    opts.explicitFiles = {synthPath("src/chain.cpp")};
    opts.symhowVersion = "test";

    const int rc = runUpdate(opts);
    REQUIRE(rc == 2);
}

TEST_CASE("update_driver: refuses a database with symhow_update_in_progress set", "[fixture][update_driver]")
{
    const std::string path = copyFixture("in_progress");
    {
        symhow::db::WritableDb db(path, false);
        db.begin();
        symhow::db::setMeta(db, "symhow_update_in_progress", "1");
        db.commit();
    }

    UpdateRunOptions opts;
    opts.dbPath = path;
    opts.cdbPath = fixtureCdbPath();
    opts.explicitFiles = {synthPath("src/chain.cpp")};
    opts.symhowVersion = "test";

    const int rc = runUpdate(opts);
    REQUIRE(rc == 2);
}

#ifdef SYMHOW_BINARY
TEST_CASE("update_driver: `symhow update --json` CLI smoke emits symhow.update-summary/1", "[fixture][update_driver]")
{
    const std::string path = copyFixture("cli_smoke");

    const auto result = symhow::common::process::run(
        {SYMHOW_BINARY, "update", "-d", path, "-c", fixtureCdbPath(), "--files", synthPath("src/chain.cpp"), "--json"});
    REQUIRE(result.exitCode == 0);

    const nlohmann::json j = nlohmann::json::parse(result.out);
    REQUIRE(j.at("schema").get<std::string>() == "symhow.update-summary/1");
    REQUIRE(j.contains("files"));
    REQUIRE(j.at("files").contains("cleared_paths"));
    REQUIRE(j.contains("gc"));
    REQUIRE(j.contains("tus"));
}
#endif
