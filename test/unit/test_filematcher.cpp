#include <catch2/catch_test_macros.hpp>

#include <algorithm>
#include <filesystem>
#include <fstream>
#include <unistd.h>

#include "kg/update/FileMatcher.h"

using kg::index::CompileEntry;
using kg::update::ChangedFile;
using kg::update::ChangeKind;
using kg::update::MatchInput;
using kg::update::MatchResult;
using kg::update::matchChanged;
using kg::update::normalizePath;
using kg::update::selectEntries;

namespace
{
bool contains(const std::vector<std::string>& v, const std::string& s)
{
    return std::find(v.begin(), v.end(), s) != v.end();
}

CompileEntry entryFor(const std::string& file)
{
    CompileEntry e;
    e.directory = "/repo";
    e.file = file;
    e.args = {"clang++", file};
    return e;
}
}  // namespace

TEST_CASE("filematcher: normalizePath collapses ./ and ../ for nonexistent paths")
{
    REQUIRE(normalizePath("/repo/./a/../b.cpp") == "/repo/b.cpp");
    REQUIRE(normalizePath("/repo/b.cpp") == "/repo/b.cpp");
}

TEST_CASE("filematcher: normalization cache preserves spelling and is shared across matching and selection")
{
    kg::common::paths::NormalizationCache cache;
    MatchInput in;
    in.dbFilePaths = {"/repo/./a.cpp"};
    in.cdb = {entryFor("/repo/./a.cpp"), entryFor("/repo/./a.cpp")};
    in.changes = {ChangedFile{ChangeKind::Modified, "", "/repo/a.cpp"}};
    REQUIRE(matchChanged(in, cache).changedInDb == std::vector<std::string>{"/repo/./a.cpp"});
    REQUIRE(cache.size() == 2);
    REQUIRE(selectEntries(in.cdb, {"/repo/a.cpp"}, cache).size() == 2);
    REQUIRE(cache.size() == 2);
}

TEST_CASE("filematcher: normalization cache is operation scoped and resolves symlinks")
{
    const auto dir = std::filesystem::temp_directory_path() / ("kg_path_cache_" + std::to_string(::getpid()));
    std::filesystem::remove_all(dir);
    std::filesystem::create_directories(dir);
    struct Cleanup
    {
        std::filesystem::path path;
        ~Cleanup() { std::filesystem::remove_all(path); }
    } cleanup{dir};
    std::ofstream(dir / "a.cpp") << "int a;";
    std::ofstream(dir / "b.cpp") << "int b;";
    const auto link = dir / "link.cpp";
    std::filesystem::create_symlink(dir / "a.cpp", link);

    kg::common::paths::NormalizationCache first;
    REQUIRE(first(link.string()) == (dir / "a.cpp").string());
    REQUIRE(first((dir / "./link.cpp").string()) == (dir / "a.cpp").string());
    REQUIRE(first((dir / "missing/../gone.cpp").string()) == (dir / "gone.cpp").string());

    std::filesystem::remove(link);
    std::filesystem::create_symlink(dir / "b.cpp", link);
    REQUIRE(first(link.string()) == (dir / "a.cpp").string()); // memoized for this operation
    kg::common::paths::NormalizationCache next;
    REQUIRE(next(link.string()) == (dir / "b.cpp").string()); // no stale cross-run cache
}

TEST_CASE("filematcher: Modified in DB -> changedInDb, spelled as the DB's original")
{
    MatchInput in;
    in.dbFilePaths = {"/repo/./a.cpp"};  // DB spelling includes a redundant "./"
    in.changes = {ChangedFile{ChangeKind::Modified, "", "/repo/a.cpp"}};

    const MatchResult r = matchChanged(in);
    REQUIRE(r.changedInDb.size() == 1);
    REQUIRE(r.changedInDb[0] == "/repo/./a.cpp");  // original DB spelling preserved
    REQUIRE(r.newTus.empty());
    REQUIRE(r.deleted.empty());
    REQUIRE(r.unmatched.empty());
}

TEST_CASE("filematcher: Added matching a CDB entry (not yet in DB) -> newTus, spelled as the CDB's original")
{
    MatchInput in;
    in.cdb = {entryFor("/repo/./new.cpp")};
    in.changes = {ChangedFile{ChangeKind::Added, "", "/repo/new.cpp"}};

    const MatchResult r = matchChanged(in);
    REQUIRE(r.newTus.size() == 1);
    REQUIRE(r.newTus[0] == "/repo/./new.cpp");
    REQUIRE(r.changedInDb.empty());
}

TEST_CASE("filematcher: Added with neither a DB row nor a CDB entry -> unmatched")
{
    MatchInput in;
    in.changes = {ChangedFile{ChangeKind::Added, "", "/repo/README.md"}};

    const MatchResult r = matchChanged(in);
    REQUIRE(r.unmatched.size() == 1);
    REQUIRE(r.unmatched[0] == "/repo/README.md");
}

TEST_CASE("filematcher: Deleted present in DB -> deleted; Deleted absent from DB -> ignored")
{
    MatchInput in;
    in.dbFilePaths = {"/repo/gone.cpp"};
    in.changes = {
        ChangedFile{ChangeKind::Deleted, "", "/repo/gone.cpp"},
        ChangedFile{ChangeKind::Deleted, "", "/repo/never-indexed.cpp"},
    };

    const MatchResult r = matchChanged(in);
    REQUIRE(r.deleted.size() == 1);
    REQUIRE(r.deleted[0] == "/repo/gone.cpp");
}

TEST_CASE("filematcher: Renamed -> old path goes through the deleted check, new path through the changed/new check")
{
    MatchInput in;
    in.dbFilePaths = {"/repo/old.cpp"};
    in.changes = {ChangedFile{ChangeKind::Renamed, "/repo/old.cpp", "/repo/new.cpp"}};

    SECTION("new path unmatched (no DB row, no CDB entry)")
    {
        const MatchResult r = matchChanged(in);
        REQUIRE(contains(r.deleted, "/repo/old.cpp"));
        REQUIRE(contains(r.unmatched, "/repo/new.cpp"));
    }

    SECTION("new path matches a CDB entry -> newTus")
    {
        in.cdb = {entryFor("/repo/new.cpp")};
        const MatchResult r = matchChanged(in);
        REQUIRE(contains(r.deleted, "/repo/old.cpp"));
        REQUIRE(contains(r.newTus, "/repo/new.cpp"));
    }

    SECTION("new path already in the DB too -> changedInDb (e.g. renamed onto an existing tracked path)")
    {
        in.dbFilePaths.insert("/repo/new.cpp");
        const MatchResult r = matchChanged(in);
        REQUIRE(contains(r.deleted, "/repo/old.cpp"));
        REQUIRE(contains(r.changedInDb, "/repo/new.cpp"));
    }
}

TEST_CASE("filematcher: selectEntries returns only entries whose normalized file is in the set")
{
    std::vector<CompileEntry> entries = {entryFor("/repo/a.cpp"), entryFor("/repo/./b.cpp"), entryFor("/repo/c.cpp")};
    std::unordered_set<std::string> wanted = {normalizePath("/repo/a.cpp"), normalizePath("/repo/b.cpp")};

    const auto selected = selectEntries(entries, wanted);
    REQUIRE(selected.size() == 2);
    std::vector<std::string> files;
    for (const auto& e : selected)
    {
        files.push_back(e.file);
    }
    REQUIRE(contains(files, "/repo/a.cpp"));
    REQUIRE(contains(files, "/repo/./b.cpp"));
}
