#include <catch2/catch_test_macros.hpp>

#include <filesystem>
#include <memory>

#include <sqlite3.h>

#include "kg/db/WritableDb.h"
#include "kg/index/Provenance.h"

using kg::db::WritableDb;
using kg::index::Provenance;

namespace
{

std::string tempDbPath()
{
    return (std::filesystem::temp_directory_path() / "kg_test_provenance.sqlite").string();
}

std::unique_ptr<WritableDb> freshMetaDb(const std::string& path)
{
    std::filesystem::remove(path);
    sqlite3* raw = nullptr;
    REQUIRE(sqlite3_open_v2(path.c_str(), &raw, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, nullptr) == SQLITE_OK);
    sqlite3_close(raw);

    auto db = std::make_unique<WritableDb>(path);
    // Minimal stand-in for the real Sourcetrail-schema `meta` table (id, key,
    // value -- no UNIQUE on key), per the Phase 1 spec.
    db->exec("CREATE TABLE meta(id INTEGER PRIMARY KEY, key TEXT, value TEXT)");
    return db;
}

}  // namespace

TEST_CASE("provenance: readProvenance returns nullopt when kg_provenance_version is absent")
{
    const std::string path = tempDbPath();
    auto db = freshMetaDb(path);
    REQUIRE_FALSE(kg::index::readProvenance(db->handle()).has_value());
    std::filesystem::remove(path);
}

TEST_CASE("provenance: write/read round-trips every field")
{
    const std::string path = tempDbPath();
    auto db = freshMetaDb(path);

    Provenance prov;
    prov.kgVersion = "1.2.3";
    prov.walkerVersion = kg::index::kWalkerVersion;
    prov.cdbPath = "/abs/path/compile_commands.json";
    prov.dbOptions.remaps.emplace_back("/old", "/new");
    prov.dbOptions.includeGlobs.push_back("src/**");
    prov.dbOptions.excludeGlobs.push_back("third_party/**");
    prov.sanitizerOptions.dropArgPatterns.push_back("-W*");
    prov.sanitizerOptions.extraArgs.push_back("-DFOO=1");
    prov.sanitizerOptions.resourceDir = "/opt/clang/lib/clang/21";
    prov.sanitizerOptions.gccToolchain = "/opt/gcc";
    prov.sanitizerOptions.stdFallback = "c++20";
    prov.sanitizerOptions.cStdFallback = "gnu11";
    prov.sanitizerOptions.cudaPath = "/opt/cuda";
    prov.sanitizerOptions.cudaMode = kg::index::cuda::CudaMode::NoWrapper;
    prov.systemFileRows = kg::index::SystemFileRows::All;
    prov.limit = 42;
    prov.repoRoot = "/home/user/repo";
    prov.repoCommit = "deadbeefcafef00d";
    prov.dirtyPaths = {"/home/user/repo/a.cpp", "/home/user/repo/b.h"};
    prov.indexedAtUtc = "2026-01-01T00:00:00Z";
    prov.lastUpdateAtUtc = "2026-01-02T00:00:00Z";

    db->begin();
    kg::index::writeProvenance(*db, prov);
    db->commit();

    const auto readBack = kg::index::readProvenance(db->handle());
    REQUIRE(readBack.has_value());
    REQUIRE(readBack->kgVersion == prov.kgVersion);
    REQUIRE(readBack->walkerVersion == prov.walkerVersion);
    REQUIRE(readBack->cdbPath == prov.cdbPath);
    REQUIRE(readBack->dbOptions.remaps == prov.dbOptions.remaps);
    REQUIRE(readBack->dbOptions.includeGlobs == prov.dbOptions.includeGlobs);
    REQUIRE(readBack->dbOptions.excludeGlobs == prov.dbOptions.excludeGlobs);
    REQUIRE(readBack->sanitizerOptions.dropArgPatterns == prov.sanitizerOptions.dropArgPatterns);
    REQUIRE(readBack->sanitizerOptions.extraArgs == prov.sanitizerOptions.extraArgs);
    REQUIRE(readBack->sanitizerOptions.resourceDir == prov.sanitizerOptions.resourceDir);
    REQUIRE(readBack->sanitizerOptions.gccToolchain == prov.sanitizerOptions.gccToolchain);
    REQUIRE(readBack->sanitizerOptions.stdFallback == prov.sanitizerOptions.stdFallback);
    REQUIRE(readBack->sanitizerOptions.cStdFallback == prov.sanitizerOptions.cStdFallback);
    REQUIRE(readBack->sanitizerOptions.cudaPath == prov.sanitizerOptions.cudaPath);
    REQUIRE(readBack->sanitizerOptions.cudaMode == prov.sanitizerOptions.cudaMode);
    REQUIRE(readBack->systemFileRows == prov.systemFileRows);
    REQUIRE(readBack->limit == prov.limit);
    REQUIRE(readBack->repoRoot == prov.repoRoot);
    REQUIRE(readBack->repoCommit == prov.repoCommit);
    REQUIRE(readBack->dirtyPaths == prov.dirtyPaths);
    REQUIRE(readBack->indexedAtUtc == prov.indexedAtUtc);
    REQUIRE(readBack->lastUpdateAtUtc == prov.lastUpdateAtUtc);

    std::filesystem::remove(path);
}

TEST_CASE("provenance: writeProvenance is idempotent (re-writing overwrites, not duplicates, meta rows)")
{
    const std::string path = tempDbPath();
    auto db = freshMetaDb(path);

    Provenance prov;
    prov.kgVersion = "1.0.0";
    prov.indexedAtUtc = "2026-01-01T00:00:00Z";

    db->begin();
    kg::index::writeProvenance(*db, prov);
    prov.kgVersion = "2.0.0";
    kg::index::writeProvenance(*db, prov);
    db->commit();

    int rows = -1;
    db->query(
        "SELECT count(*) FROM meta WHERE key = 'kg_version'",
        [&](sqlite3_stmt* stmt) { rows = sqlite3_column_int(stmt, 0); });
    REQUIRE(rows == 1);

    const auto readBack = kg::index::readProvenance(db->handle());
    REQUIRE(readBack.has_value());
    REQUIRE(readBack->kgVersion == "2.0.0");

    std::filesystem::remove(path);
}
