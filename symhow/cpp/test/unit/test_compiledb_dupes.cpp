// Unit coverage for symhow::index::pathArgsHashes() -- the combined, per-path
// args_hash/entry_count recipe that fixes the duplicate-CDB-entry bug (see
// docs/KNOWN_LIMITATIONS.md): a real compile_commands.json can list the same
// `file` more than once (several targets/configs with different flags), but
// symhow_tu is keyed by PATH, so a single, order-independent hash has to stand
// in for however many entries that path actually has.

#include <catch2/catch_test_macros.hpp>

#include <cctype>
#include <string>
#include <vector>

#include "symhow/index/CompileDb.h"

using symhow::index::CompileEntry;
using symhow::index::pathArgsHashes;
using symhow::index::PathArgsInfo;

namespace
{

CompileEntry makeEntry(const std::string& file, std::vector<std::string> args, std::string directory = "/work")
{
    CompileEntry e;
    e.directory = std::move(directory);
    e.file = file;
    e.args = std::move(args);
    return e;
}

bool isHex16(const std::string& s)
{
    if (s.size() != 16)
    {
        return false;
    }
    for (char c : s)
    {
        if (!std::isxdigit(static_cast<unsigned char>(c)))
        {
            return false;
        }
    }
    return true;
}

}  // namespace

TEST_CASE("pathArgsHashes: a single-entry path gets a 16-hex-char hash and entry_count 1", "[cdb][dupes]")
{
    const std::vector<CompileEntry> entries = {makeEntry("/work/a.cpp", {"/usr/bin/c++", "-DFOO=1", "-c", "a.cpp"})};
    const auto result = pathArgsHashes(entries);
    REQUIRE(result.size() == 1);
    const auto it = result.find("/work/a.cpp");
    REQUIRE(it != result.end());
    CHECK(isHex16(it->second.combinedHash));
    CHECK(it->second.entryCount == 1);
}

TEST_CASE("pathArgsHashes: a duplicated path's combined hash is independent of CDB entry order", "[cdb][dupes]")
{
    const CompileEntry e1 = makeEntry("/work/a.cpp", {"/usr/bin/c++", "-DFOO=1", "-c", "a.cpp"});
    const CompileEntry e2 = makeEntry("/work/a.cpp", {"/usr/bin/c++", "-DBAR=2", "-c", "a.cpp"});

    const std::vector<CompileEntry> forward = {e1, e2};
    const std::vector<CompileEntry> reversed = {e2, e1};

    const auto forwardResult = pathArgsHashes(forward);
    const auto reversedResult = pathArgsHashes(reversed);

    REQUIRE(forwardResult.size() == 1);
    REQUIRE(reversedResult.size() == 1);

    const auto& forwardInfo = forwardResult.at("/work/a.cpp");
    const auto& reversedInfo = reversedResult.at("/work/a.cpp");

    CHECK(forwardInfo.combinedHash == reversedInfo.combinedHash);
    CHECK(forwardInfo.entryCount == 2);
    CHECK(reversedInfo.entryCount == 2);
    CHECK(isHex16(forwardInfo.combinedHash));
}

TEST_CASE("pathArgsHashes: the combined hash changes when ANY one entry's flags change", "[cdb][dupes]")
{
    const CompileEntry e1 = makeEntry("/work/a.cpp", {"/usr/bin/c++", "-DFOO=1", "-c", "a.cpp"});
    const CompileEntry e2 = makeEntry("/work/a.cpp", {"/usr/bin/c++", "-DBAR=2", "-c", "a.cpp"});
    const CompileEntry e2Changed = makeEntry("/work/a.cpp", {"/usr/bin/c++", "-DBAR=3", "-c", "a.cpp"});

    const auto before = pathArgsHashes({e1, e2});
    const auto after = pathArgsHashes({e1, e2Changed});

    CHECK(before.at("/work/a.cpp").combinedHash != after.at("/work/a.cpp").combinedHash);
    // entry_count is unaffected -- still two entries either way.
    CHECK(before.at("/work/a.cpp").entryCount == 2);
    CHECK(after.at("/work/a.cpp").entryCount == 2);
}

TEST_CASE("pathArgsHashes: two entries with byte-identical args still count as entry_count 2, not 1", "[cdb][dupes]")
{
    // Same file, same flags, compiled twice (e.g. two targets that happen
    // to share an identical compile line) -- entry_count must reflect the
    // real number of CDB entries, even though the DEDUPED hash set behind
    // combinedHash collapses to a single string.
    const CompileEntry e1 = makeEntry("/work/a.cpp", {"/usr/bin/c++", "-DFOO=1", "-c", "a.cpp"});
    const CompileEntry e2 = makeEntry("/work/a.cpp", {"/usr/bin/c++", "-DFOO=1", "-c", "a.cpp"});

    const auto result = pathArgsHashes({e1, e2});
    REQUIRE(result.size() == 1);
    CHECK(result.at("/work/a.cpp").entryCount == 2);

    // And that combined hash must equal the single-entry case's hash for an
    // entry with the same args (deduping two identical hashes reduces to
    // hashing just one string either way).
    const auto single = pathArgsHashes({e1});
    CHECK(result.at("/work/a.cpp").combinedHash == single.at("/work/a.cpp").combinedHash);
}

TEST_CASE("pathArgsHashes: distinct paths never collide, and each keeps its own count", "[cdb][dupes]")
{
    const std::vector<CompileEntry> entries = {
        makeEntry("/work/a.cpp", {"/usr/bin/c++", "-c", "a.cpp"}),
        makeEntry("/work/b.cpp", {"/usr/bin/c++", "-c", "b.cpp"}),
        makeEntry("/work/b.cpp", {"/usr/bin/c++", "-DX=1", "-c", "b.cpp"}),
    };
    const auto result = pathArgsHashes(entries);
    REQUIRE(result.size() == 2);
    CHECK(result.at("/work/a.cpp").entryCount == 1);
    CHECK(result.at("/work/b.cpp").entryCount == 2);
    CHECK(result.at("/work/a.cpp").combinedHash != result.at("/work/b.cpp").combinedHash);
}
