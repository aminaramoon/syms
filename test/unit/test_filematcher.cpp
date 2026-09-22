#include <catch2/catch_test_macros.hpp>

#include <algorithm>

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
