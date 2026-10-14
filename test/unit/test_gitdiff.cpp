#include <catch2/catch_test_macros.hpp>

#include <filesystem>
#include <string>

#include "symhow/update/GitDiff.h"

using symhow::update::ChangeKind;
using symhow::update::parseNameStatusZ;
using symhow::update::parseStatusPorcelainZ;

namespace
{
const std::filesystem::path kRoot = "/repo";

// Builds a NUL-separated byte string from `tokens`, terminated by a
// trailing NUL (matching real `git ... -z` output) -- avoids the
// error-prone business of hand-counting bytes for std::string's
// (const char*, size_t) constructor.
std::string nulJoin(std::initializer_list<std::string> tokens)
{
    std::string out;
    for (const std::string& t : tokens)
    {
        out += t;
        out += '\0';
    }
    return out;
}
}  // namespace

TEST_CASE("gitdiff: parseNameStatusZ handles A/M/D/T")
{
    const std::string in = nulJoin({"A", "a.cpp", "M", "b.cpp", "D", "c.cpp", "T", "d.cpp"});
    const auto changes = parseNameStatusZ(in, kRoot);
    REQUIRE(changes.size() == 4);
    REQUIRE(changes[0].kind == ChangeKind::Added);
    REQUIRE(changes[0].newPath == "/repo/a.cpp");
    REQUIRE(changes[0].oldPath.empty());
    REQUIRE(changes[1].kind == ChangeKind::Modified);
    REQUIRE(changes[1].newPath == "/repo/b.cpp");
    REQUIRE(changes[2].kind == ChangeKind::Deleted);
    REQUIRE(changes[2].newPath == "/repo/c.cpp");
    REQUIRE(changes[3].kind == ChangeKind::Modified);
    REQUIRE(changes[3].newPath == "/repo/d.cpp");
}

TEST_CASE("gitdiff: parseNameStatusZ handles renames (R<score>) with both old and new paths")
{
    const std::string in = nulJoin({"R100", "old.cpp", "new.cpp"});
    const auto changes = parseNameStatusZ(in, kRoot);
    REQUIRE(changes.size() == 1);
    REQUIRE(changes[0].kind == ChangeKind::Renamed);
    REQUIRE(changes[0].oldPath == "/repo/old.cpp");
    REQUIRE(changes[0].newPath == "/repo/new.cpp");
}

TEST_CASE("gitdiff: parseNameStatusZ handles copies (C<score>) as Added(newPath)")
{
    const std::string in = nulJoin({"C75", "src.cpp", "copy.cpp"});
    const auto changes = parseNameStatusZ(in, kRoot);
    REQUIRE(changes.size() == 1);
    REQUIRE(changes[0].kind == ChangeKind::Added);
    REQUIRE(changes[0].oldPath.empty());
    REQUIRE(changes[0].newPath == "/repo/copy.cpp");
}

TEST_CASE("gitdiff: parseNameStatusZ handles paths with spaces")
{
    const std::string in = nulJoin({"M", "path with spaces.cpp"});
    const auto changes = parseNameStatusZ(in, kRoot);
    REQUIRE(changes.size() == 1);
    REQUIRE(changes[0].newPath == "/repo/path with spaces.cpp");
}

TEST_CASE("gitdiff: parseNameStatusZ tolerates a trailing NUL and empty input")
{
    const std::string trailing = nulJoin({"A", "a.cpp"});
    REQUIRE(parseNameStatusZ(trailing, kRoot).size() == 1);

    REQUIRE(parseNameStatusZ(std::string_view{}, kRoot).empty());
}

TEST_CASE("gitdiff: parseNameStatusZ normalizes ./ and ../ segments")
{
    const std::string in = nulJoin({"M", "./sub/../a.cpp"});
    const auto changes = parseNameStatusZ(in, kRoot);
    REQUIRE(changes.size() == 1);
    REQUIRE(changes[0].newPath == "/repo/a.cpp");
}

TEST_CASE("gitdiff: parseStatusPorcelainZ handles a plain modified entry")
{
    const std::string in = nulJoin({" M a.cpp"});
    const auto paths = parseStatusPorcelainZ(in, kRoot);
    REQUIRE(paths.size() == 1);
    REQUIRE(paths[0] == "/repo/a.cpp");
}

TEST_CASE("gitdiff: parseStatusPorcelainZ handles untracked entries")
{
    const std::string in = nulJoin({"?? new.cpp"});
    const auto paths = parseStatusPorcelainZ(in, kRoot);
    REQUIRE(paths.size() == 1);
    REQUIRE(paths[0] == "/repo/new.cpp");
}

TEST_CASE("gitdiff: parseStatusPorcelainZ handles renames (R  new\\0old) and only reports the new path")
{
    const std::string in = nulJoin({"R  new", "old"});
    const auto paths = parseStatusPorcelainZ(in, kRoot);
    REQUIRE(paths.size() == 1);
    REQUIRE(paths[0] == "/repo/new");
}

TEST_CASE("gitdiff: parseStatusPorcelainZ skips ignored (!!) entries")
{
    const std::string in = nulJoin({" M a.cpp", "!! ignored.o", "?? b.cpp"});
    const auto paths = parseStatusPorcelainZ(in, kRoot);
    REQUIRE(paths.size() == 2);
    REQUIRE(paths[0] == "/repo/a.cpp");
    REQUIRE(paths[1] == "/repo/b.cpp");
}

TEST_CASE("gitdiff: parseStatusPorcelainZ tolerates empty input")
{
    REQUIRE(parseStatusPorcelainZ(std::string_view{}, kRoot).empty());
}

TEST_CASE("gitdiff: parseNameStatusZ maps U (unmerged) to ChangeKind::Unmerged, not Modified")
{
    const std::string in = nulJoin({"U", "conflicted.cpp"});
    const auto changes = parseNameStatusZ(in, kRoot);
    REQUIRE(changes.size() == 1);
    REQUIRE(changes[0].kind == ChangeKind::Unmerged);
    REQUIRE(changes[0].newPath == "/repo/conflicted.cpp");
}

TEST_CASE("gitdiff: statusHasUnmergedEntries detects every unmerged XY pairing")
{
    using symhow::update::statusHasUnmergedEntries;
    REQUIRE(statusHasUnmergedEntries(nulJoin({"UU conflicted.cpp"})));
    REQUIRE(statusHasUnmergedEntries(nulJoin({"AU added_by_us.cpp"})));
    REQUIRE(statusHasUnmergedEntries(nulJoin({"UA added_by_them.cpp"})));
    REQUIRE(statusHasUnmergedEntries(nulJoin({"DU deleted_by_us.cpp"})));
    REQUIRE(statusHasUnmergedEntries(nulJoin({"UD deleted_by_them.cpp"})));
}

TEST_CASE("gitdiff: statusHasUnmergedEntries is false for ordinary entries")
{
    using symhow::update::statusHasUnmergedEntries;
    REQUIRE_FALSE(statusHasUnmergedEntries(nulJoin({" M a.cpp", "?? b.cpp", "A  c.cpp"})));
    REQUIRE_FALSE(statusHasUnmergedEntries(std::string_view{}));
}
