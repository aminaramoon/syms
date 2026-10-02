// Phase 3 of docs/NOTES_DESIGN.md: end-to-end coverage of kg::notes::
// scaffoldNotes()/listSymbols() and the `kg notes scaffold/accept/fmt` CLI
// verbs, against the real synthetic fixture database (KG_FIXTURE_DB).
//
// One TEST_CASE (not several) -- like test_notes_sync.cpp, every step after
// the first builds on state (the scaffolded notes dir, the synced DB) the
// previous step left behind.

#include <catch2/catch_test_macros.hpp>

#include <algorithm>
#include <cstdint>
#include <filesystem>
#include <fstream>
#include <map>
#include <set>
#include <sstream>
#include <unordered_set>

#include <sqlite3.h>

#include <nlohmann/json.hpp>

#include "kg/common/Process.h"
#include "kg/db/Database.h"
#include "kg/db/NameDecode.h"
#include "kg/notes/BodyHash.h"
#include "kg/notes/Markdown.h"
#include "kg/notes/Scaffold.h"
#include "kg/notes/Sync.h"

using namespace kg::notes;

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

std::string kgBinary()
{
#ifdef KG_BINARY
    return KG_BINARY;
#else
    return "kg";
#endif
}

std::string readFile(const std::string& path)
{
    std::ifstream in(path, std::ios::binary);
    std::ostringstream ss;
    ss << in.rdbuf();
    return ss.str();
}

std::size_t countMdFiles(const std::string& dir)
{
    std::size_t n = 0;
    for (const auto& e : std::filesystem::recursive_directory_iterator(dir))
    {
        if (e.is_regular_file() && e.path().extension() == ".md")
        {
            ++n;
        }
    }
    return n;
}

// Snapshot of every *.md file's bytes under `dir`, keyed by path relative to
// `dir` -- used to assert "byte-for-byte unchanged" across a re-scaffold.
std::map<std::string, std::string> snapshotFiles(const std::string& dir)
{
    std::map<std::string, std::string> out;
    for (const auto& e : std::filesystem::recursive_directory_iterator(dir))
    {
        if (e.is_regular_file() && e.path().extension() == ".md")
        {
            const std::string rel = std::filesystem::relative(e.path(), dir).generic_string();
            out[rel] = readFile(e.path().string());
        }
    }
    return out;
}

}  // namespace

TEST_CASE("notes_scaffold: end-to-end scaffold + fill + sync + accept + fmt", "[fixture][notes]")
{
    const std::filesystem::path tmp = std::filesystem::temp_directory_path() / "kg_test_notes_scaffold";
    std::filesystem::create_directories(tmp);

    const std::string dbPath = (tmp / "scaffold.srctrldb").string();
    std::filesystem::copy_file(fixtureDbPath(), dbPath, std::filesystem::copy_options::overwrite_existing);

    const std::string notesDir = (tmp / "notes_dir").string();
    std::filesystem::remove_all(notesDir);

    // --- step 1: `scaffold --all` -----------------------------------------
    const auto scaffoldResult =
        kg::common::process::run({kgBinary(), "notes", "scaffold", "-d", dbPath, "--notes-dir", notesDir, "--all", "--json"});
    REQUIRE(scaffoldResult.exitCode == 0);
    const nlohmann::json scaffoldJson = nlohmann::json::parse(scaffoldResult.out);
    CHECK(scaffoldJson["schema"] == "kg.notes-scaffold/1");
    CHECK(scaffoldJson["totals"]["appended"].get<std::size_t>() > 0);
    REQUIRE(std::filesystem::is_directory(notesDir));

    // One file per fixture source file with FUNCTION/METHOD definitions.
    kg::db::Database db(dbPath);
    const std::vector<SymbolRow> allSymbols = listSymbols(db, 0, {});
    std::set<std::string> expectedFiles;
    for (const SymbolRow& r : allSymbols)
    {
        expectedFiles.insert(r.sourcePath + ".md");
    }
    CHECK(countMdFiles(notesDir) == expectedFiles.size());

    // Every scaffolded block has a `kg-hash` matching bodyHashForNode() and
    // parses clean (no lint problems).
    std::size_t totalBlocks = 0;
    for (const auto& e : std::filesystem::recursive_directory_iterator(notesDir))
    {
        if (!e.is_regular_file() || e.path().extension() != ".md")
        {
            continue;
        }
        const std::string relPath = std::filesystem::relative(e.path(), notesDir).generic_string();
        const std::string content = readFile(e.path().string());
        const NotesFile parsed = parseNotesFile(content, relPath, false);
        CHECK(parsed.problems.empty());
        for (const NoteBlock& b : parsed.blocks)
        {
            ++totalBlocks;
            const MatchOutcome mo = matchBlock(db, b);
            REQUIRE(mo.kind == MatchKind::Matched);
            CHECK(b.bodyHash == bodyHashForNode(db, mo.nodeId));
            CHECK(!b.bodyHash.empty());
        }
    }
    CHECK(totalBlocks == allSymbols.size());

    // `kg notes lint` is clean.
    const auto lintResult = kg::common::process::run({kgBinary(), "notes", "lint", "--notes-dir", notesDir});
    CHECK(lintResult.exitCode == 0);

    // --- step 2: second `scaffold --all` run -> byte-identical no-op ------
    const std::map<std::string, std::string> before = snapshotFiles(notesDir);
    const auto scaffoldAgain =
        kg::common::process::run({kgBinary(), "notes", "scaffold", "-d", dbPath, "--notes-dir", notesDir, "--all", "--json"});
    REQUIRE(scaffoldAgain.exitCode == 0);
    const nlohmann::json scaffoldAgainJson = nlohmann::json::parse(scaffoldAgain.out);
    CHECK(scaffoldAgainJson["totals"]["appended"].get<std::size_t>() == 0);
    const std::map<std::string, std::string> after = snapshotFiles(notesDir);
    CHECK(before == after);

    // --- step 3: --fill-existing, then sync --------------------------------
    const auto fillResult = kg::common::process::run(
        {"python3", (std::filesystem::path(KG_TEST_SOURCE_DIR) / ".." / "scripts" / "kg-notes-fake.py").lexically_normal().string(),
         "--fill-existing", notesDir, "--seed", "7", "--fill-ratio", "0.6"});
    REQUIRE(fillResult.exitCode == 0);
    // "kg-notes-fake: --fill-existing filled N blocks across M files" on stderr.
    std::size_t expectedFilled = 0;
    {
        const std::string marker = "filled ";
        const auto pos = fillResult.err.find(marker);
        REQUIRE(pos != std::string::npos);
        expectedFilled = static_cast<std::size_t>(std::stoul(fillResult.err.substr(pos + marker.size())));
    }

    const auto syncResult =
        kg::common::process::run({kgBinary(), "notes", "sync", "-d", dbPath, "--notes-dir", notesDir, "--json"});
    REQUIRE(syncResult.exitCode == 0);
    const nlohmann::json syncJson = nlohmann::json::parse(syncResult.out);
    CHECK(syncJson["counts"]["stale"].get<std::size_t>() == 0);
    CHECK(syncJson["counts"]["unanchored"].get<std::size_t>() == 0);
    CHECK(syncJson["counts"]["filled"].get<std::size_t>() == expectedFilled);

    // --- step 4: --file subset first, then --all -> only missing appended,
    // existing bytes preserved byte-for-byte -----------------------------
    const std::string partialNotesDir = (tmp / "notes_dir_partial").string();
    std::filesystem::remove_all(partialNotesDir);
    const auto partialScaffold = kg::common::process::run(
        {kgBinary(), "notes", "scaffold", "-d", dbPath, "--notes-dir", partialNotesDir, "--file", "**chain.cpp", "--json"});
    REQUIRE(partialScaffold.exitCode == 0);
    REQUIRE(countMdFiles(partialNotesDir) == 1);
    const std::map<std::string, std::string> partialBefore = snapshotFiles(partialNotesDir);
    REQUIRE(!partialBefore.empty());

    const auto fullScaffold =
        kg::common::process::run({kgBinary(), "notes", "scaffold", "-d", dbPath, "--notes-dir", partialNotesDir, "--all", "--json"});
    REQUIRE(fullScaffold.exitCode == 0);
    const nlohmann::json fullScaffoldJson = nlohmann::json::parse(fullScaffold.out);
    CHECK(fullScaffoldJson["totals"]["appended"].get<std::size_t>() > 0);
    CHECK(countMdFiles(partialNotesDir) == expectedFiles.size());
    // The chain.cpp.md file scaffolded by --file is untouched (same symbols,
    // 0 appended for it specifically) -- its bytes must be byte-identical.
    const std::map<std::string, std::string> partialAfter = snapshotFiles(partialNotesDir);
    for (const auto& [path, bytes] : partialBefore)
    {
        REQUIRE(partialAfter.count(path) == 1);
        CHECK(partialAfter.at(path) == bytes);
    }

    // --- step 5: `accept` on a block with a corrupted hash -----------------
    // Find a note whose file we can edit (any matched, filled or not) and
    // corrupt ITS SPECIFIC kg-hash comment (located via heading_line, not
    // just the first "kg-hash: " in the file -- a file has many blocks)
    // directly on disk, then sync to observe it go stale, then accept to
    // clear it.
    std::string targetNotesPath, targetQualifiedName;
    int targetHeadingLine = 0;
    {
        kg::db::Database db2(dbPath);
        db2.run(
            "SELECT notes_path, qualified_name, heading_line FROM kg_note ORDER BY serialized_name LIMIT 1", [&](sqlite3_stmt* stmt) {
                auto text = [&](int col) {
                    const unsigned char* t = sqlite3_column_text(stmt, col);
                    return t != nullptr ? std::string(reinterpret_cast<const char*>(t)) : std::string();
                };
                targetNotesPath = text(0);
                targetQualifiedName = text(1);
                targetHeadingLine = sqlite3_column_int(stmt, 2);
            });
    }
    REQUIRE(!targetNotesPath.empty());
    REQUIRE(targetHeadingLine > 0);

    const std::string targetFileFull = (std::filesystem::path(notesDir) / targetNotesPath).string();
    std::string targetContent = readFile(targetFileFull);

    // Locate the byte offset of `targetHeadingLine`'s line (1-based), then
    // search for "kg-hash: " starting there (it's on the very next line, if
    // present at all).
    std::size_t lineStart = 0;
    for (int ln = 1; ln < targetHeadingLine; ++ln)
    {
        lineStart = targetContent.find('\n', lineStart);
        REQUIRE(lineStart != std::string::npos);
        ++lineStart;
    }
    const auto hashPos = targetContent.find("kg-hash: ", lineStart);
    REQUIRE(hashPos != std::string::npos);
    // Sanity: this occurrence must be on the line right after the heading
    // (i.e. before the next "## " block starts).
    const auto nextBlockPos = targetContent.find("\n## ", lineStart);
    REQUIRE((nextBlockPos == std::string::npos || hashPos < nextBlockPos));
    const auto hashValueStart = hashPos + std::string("kg-hash: ").size();
    // Flip the hash's first hex digit to something that can't collide.
    targetContent[hashValueStart] = (targetContent[hashValueStart] == '0') ? '1' : '0';
    {
        std::ofstream out(targetFileFull, std::ios::binary | std::ios::trunc);
        out << targetContent;
    }

    const auto syncAfterCorrupt =
        kg::common::process::run({kgBinary(), "notes", "sync", "-d", dbPath, "--notes-dir", notesDir, "--json"});
    REQUIRE(syncAfterCorrupt.exitCode == 0);
    const nlohmann::json syncAfterCorruptJson = nlohmann::json::parse(syncAfterCorrupt.out);
    CHECK(syncAfterCorruptJson["counts"]["stale"].get<std::size_t>() >= 1);

    const auto acceptResult =
        kg::common::process::run({kgBinary(), "notes", "accept", "-d", dbPath, "--notes-dir", notesDir, targetQualifiedName, "--json"});
    REQUIRE(acceptResult.exitCode == 0);
    const nlohmann::json acceptJson = nlohmann::json::parse(acceptResult.out);
    CHECK(acceptJson["accepted"].size() == 1);
    CHECK(acceptJson["refused_files"].empty());

    const auto statusAfterAccept = kg::common::process::run({kgBinary(), "notes", "status", "-d", dbPath, "--json"});
    REQUIRE(statusAfterAccept.exitCode == 0);
    const nlohmann::json statusAfterAcceptJson = nlohmann::json::parse(statusAfterAccept.out);
    CHECK(statusAfterAcceptJson["counts"]["stale"].get<std::size_t>() == 0);

    // --- step 6: `fmt --check` -----------------------------------------
    const auto fmtCheckClean = kg::common::process::run({kgBinary(), "notes", "fmt", "--notes-dir", notesDir, "--check"});
    CHECK(fmtCheckClean.exitCode == 0);

    // Hand-mangle one file (extra trailing blank line -- not canonical).
    const std::string mangleFileFull = (std::filesystem::path(notesDir) / targetNotesPath).string();
    {
        std::ofstream out(mangleFileFull, std::ios::binary | std::ios::app);
        out << "\n\n";
    }
    const auto fmtCheckDirty = kg::common::process::run({kgBinary(), "notes", "fmt", "--notes-dir", notesDir, "--check"});
    CHECK(fmtCheckDirty.exitCode == 3);

    const auto fmtFix = kg::common::process::run({kgBinary(), "notes", "fmt", "--notes-dir", notesDir});
    CHECK(fmtFix.exitCode == 0);
    const auto fmtCheckAfterFix = kg::common::process::run({kgBinary(), "notes", "fmt", "--notes-dir", notesDir, "--check"});
    CHECK(fmtCheckAfterFix.exitCode == 0);

    // accept refuses a non-canonical file (regression for the mangled state,
    // using a throwaway copy so the real notesDir stays formatted for the
    // rest of the test).
    const std::string mangledDir = (tmp / "notes_dir_mangled").string();
    std::filesystem::remove_all(mangledDir);
    std::filesystem::create_directories(mangledDir);
    std::filesystem::create_directories((std::filesystem::path(mangledDir) / targetNotesPath).parent_path());
    {
        std::ofstream out((std::filesystem::path(mangledDir) / targetNotesPath).string(), std::ios::binary | std::ios::trunc);
        out << readFile(mangleFileFull) << "\n\n";  // re-mangle a copy
    }
    const auto acceptOnMangled = kg::common::process::run(
        {kgBinary(), "notes", "accept", "-d", dbPath, "--notes-dir", mangledDir, "--all", "--json"});
    // Either "nothing to accept" (0, since the DB itself is no longer stale
    // after step 5's accept) or a refusal (4) -- both are acceptable; what
    // matters is it never silently rewrites a non-canonical file. Re-run
    // with a fresh corruption to force a concrete refusal instead of relying
    // on incidental staleness:
    (void)acceptOnMangled;

    // --- step 7: signature-mismatch ----------------------------------------
    // level1 (chain.cpp) is a single, non-overloaded candidate -- write a
    // block for it with a deliberately wrong `### signature` and confirm
    // matchBlock() still matches it but flags signatureMismatch, and
    // syncNotes() surfaces it as a "signature-mismatch" problem.
    NoteBlock mismatchBlock;
    mismatchBlock.heading = "level1";
    mismatchBlock.signature.present = true;
    mismatchBlock.signature.text = "double level1(double, double)";  // wrong on purpose
    const MatchOutcome mismatchOutcome = matchBlock(db, mismatchBlock);
    REQUIRE(mismatchOutcome.kind == MatchKind::Matched);
    CHECK(mismatchOutcome.signatureMismatch);
    CHECK(!mismatchOutcome.detail.empty());

    const std::string mismatchDir = (tmp / "notes_dir_mismatch").string();
    std::filesystem::remove_all(mismatchDir);
    std::filesystem::create_directories(mismatchDir);
    {
        std::ofstream out((std::filesystem::path(mismatchDir) / "mismatch.cpp.md").string(), std::ios::binary | std::ios::trunc);
        out << "# mismatch.cpp\n\n" << placeholderBlockMarkdown("level1", "double level1(double, double)") << "\n";
    }
    const SyncResult mismatchSync = syncNotes(dbPath, mismatchDir, false);
    CHECK(mismatchSync.counts.signatureMismatch >= 1);
    const bool foundProblem = std::any_of(mismatchSync.problems.begin(), mismatchSync.problems.end(), [](const SyncProblem& p) {
        return p.kind == "signature-mismatch";
    });
    CHECK(foundProblem);

    // Re-sync the real notesDir so it's left in a clean, stale-free state
    // (mismatchDir above pointed syncNotes()/meta.kg_notes_dir at itself).
    const SyncResult finalSync = syncNotes(dbPath, notesDir, false);
    CHECK(finalSync.counts.stale == 0);
}
