// Exercises kg::update::{takeSnapshot,diffSnapshots} (and the hidden
// `kg dbdiff` CLI command built on top of them) against the two hermetic
// fixture databases the rest of the suite already builds -- see
// test/CMakeLists.txt's "snapshot." registration (needs BOTH FixtureDb and
// FixtureCudaDb, unlike every other [fixture]-tagged test group).

#include <catch2/catch_test_macros.hpp>

#include <filesystem>
#include <fstream>
#include <sstream>

#include "kg/notes/Scaffold.h"
#include "kg/notes/Sync.h"
#include "kg/query/QueryEngine.h"
#include "kg/update/Snapshot.h"

#ifdef KG_BINARY
#include "kg/common/Process.h"
#endif

using kg::update::diffSnapshots;
using kg::update::takeSnapshot;

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

std::string cudaFixtureDbPath()
{
#ifdef KG_CUDA_FIXTURE_DB
    return KG_CUDA_FIXTURE_DB;
#else
    return "cuda_fixture.srctrldb";
#endif
}
}  // namespace

TEST_CASE("snapshot: diffSnapshots(take(fixture), take(fixture)) is empty (self-diff, reflexivity)", "[fixture][snapshot]")
{
    const auto a = takeSnapshot(fixtureDbPath());
    const auto b = takeSnapshot(fixtureDbPath());
    const auto diff = diffSnapshots(a, b);
    REQUIRE(diff.onlyInA.empty());
    REQUIRE(diff.onlyInB.empty());

    REQUIRE_FALSE(a.nodes.empty());  // sanity: the fixture actually has content
}

TEST_CASE("snapshot: diffSnapshots(take(fixture), take(cuda_fixture)) is non-empty and node entries are present", "[fixture][snapshot]")
{
    const auto a = takeSnapshot(fixtureDbPath());
    const auto b = takeSnapshot(cudaFixtureDbPath());
    const auto diff = diffSnapshots(a, b);

    REQUIRE_FALSE(diff.onlyInA.empty());
    REQUIRE_FALSE(diff.onlyInB.empty());

    bool sawNodeEntry = false;
    for (const std::string& s : diff.onlyInA)
    {
        if (s.rfind("node|", 0) == 0)
        {
            sawNodeEntry = true;
            break;
        }
    }
    REQUIRE(sawNodeEntry);
}

TEST_CASE("snapshot: diffSnapshots is symmetric (A\\B and B\\A swap roles when args swap)", "[fixture][snapshot]")
{
    const auto a = takeSnapshot(fixtureDbPath());
    const auto b = takeSnapshot(cudaFixtureDbPath());
    const auto ab = diffSnapshots(a, b);
    const auto ba = diffSnapshots(b, a);
    REQUIRE(ab.onlyInA.size() == ba.onlyInB.size());
    REQUIRE(ab.onlyInB.size() == ba.onlyInA.size());
}

TEST_CASE("snapshot: diffSnapshots reports a multiset count mismatch, not a silent collapse", "[snapshot]")
{
    // Snapshot's categories are multisets (key -> count), not std::set --
    // a key appearing twice on one side and once on the other must be
    // reported as a count mismatch, not treated as "present on both, so
    // equal" (which a plain std::set would do, since it collapses
    // duplicates).
    kg::update::Snapshot a;
    a.localSymbols["dup"] = 2;
    a.localSymbols["only_a"] = 1;

    kg::update::Snapshot b;
    b.localSymbols["dup"] = 1;
    b.localSymbols["only_b"] = 1;

    const auto diff = diffSnapshots(a, b);

    bool sawMismatchInA = false;
    for (const std::string& s : diff.onlyInA)
    {
        if (s == "local_symbol|dup (x2 vs x1)")
        {
            sawMismatchInA = true;
        }
    }
    REQUIRE(sawMismatchInA);

    bool sawMismatchInB = false;
    for (const std::string& s : diff.onlyInB)
    {
        if (s == "local_symbol|dup (x1 vs x2)")
        {
            sawMismatchInB = true;
        }
    }
    REQUIRE(sawMismatchInB);

    bool sawOnlyA = false;
    for (const std::string& s : diff.onlyInA)
    {
        if (s == "local_symbol|only_a")
        {
            sawOnlyA = true;
        }
    }
    REQUIRE(sawOnlyA);

    bool sawOnlyB = false;
    for (const std::string& s : diff.onlyInB)
    {
        if (s == "local_symbol|only_b")
        {
            sawOnlyB = true;
        }
    }
    REQUIRE(sawOnlyB);
}

TEST_CASE("snapshot: diffSnapshots(a, a) with duplicated keys (equal counts) is empty", "[snapshot]")
{
    kg::update::Snapshot a;
    a.nodes["dup|4096"] = 3;
    const auto diff = diffSnapshots(a, a);
    REQUIRE(diff.onlyInA.empty());
    REQUIRE(diff.onlyInB.empty());
}

#ifdef KG_BINARY
TEST_CASE("snapshot: `kg dbdiff` CLI smoke -- equal databases exit 0, different exit 3", "[fixture][snapshot]")
{
    {
        const auto result = kg::common::process::run({KG_BINARY, "dbdiff", fixtureDbPath(), fixtureDbPath()});
        REQUIRE(result.exitCode == 0);
    }
    {
        const auto result = kg::common::process::run({KG_BINARY, "dbdiff", fixtureDbPath(), cudaFixtureDbPath()});
        REQUIRE(result.exitCode == 3);
        REQUIRE_FALSE(result.out.empty());
    }
    {
        const auto result = kg::common::process::run({KG_BINARY, "dbdiff", fixtureDbPath(), "/nonexistent.srctrldb"});
        REQUIRE(result.exitCode == 4);
    }
    {
        const auto result = kg::common::process::run({KG_BINARY, "dbdiff", fixtureDbPath(), cudaFixtureDbPath(), "--json"});
        REQUIRE(result.exitCode == 3);
        REQUIRE(result.out.find("\"schema\":\"kg.dbdiff/1\"") != std::string::npos);
        REQUIRE(result.out.find("\"equal\":false") != std::string::npos);
    }
}
#endif

TEST_CASE("snapshot: --ignore shared-header-locations drops ONLY shared-header locations", "[fixture][snapshot]")
{
    const kg::update::Snapshot full = takeSnapshot(fixtureDbPath());

    kg::update::SnapshotOptions options;
    options.ignoreSharedHeaderLocations = true;
    const kg::update::Snapshot filtered = takeSnapshot(fixtureDbPath(), options);

    // Every OTHER category is untouched -- the option is not a general
    // "compare less" knob (see docs/KNOWN_LIMITATIONS.md's Phase 5 section).
    REQUIRE(filtered.nodes == full.nodes);
    REQUIRE(filtered.edges == full.edges);
    REQUIRE(filtered.symbols == full.symbols);
    REQUIRE(filtered.files == full.files);
    REQUIRE(filtered.errors == full.errors);
    REQUIRE(filtered.localSymbols == full.localSymbols);
    REQUIRE(filtered.symbolDefs == full.symbolDefs);
    REQUIRE(filtered.tus == full.tus);

    // The synthetic fixture has headers included by more than one TU
    // (shapes.hpp / ns.hpp), so something must actually have been dropped,
    // and what remains must be a strict subset of the unfiltered snapshot.
    REQUIRE(filtered.locations.size() < full.locations.size());
    for (const auto& [key, count] : filtered.locations)
    {
        const auto it = full.locations.find(key);
        REQUIRE(it != full.locations.end());
        REQUIRE(it->second == count);
    }

    // Self-diff under the same option is still empty (reflexivity holds).
    REQUIRE(diffSnapshots(filtered, filtered).onlyInA.empty());
    REQUIRE(diffSnapshots(filtered, filtered).onlyInB.empty());
}

namespace
{
std::string readFileText(const std::string& path)
{
    std::ifstream in(path, std::ios::binary);
    std::ostringstream ss;
    ss << in.rdbuf();
    return ss.str();
}

void writeFileText(const std::string& path, const std::string& content)
{
    std::ofstream out(path, std::ios::binary | std::ios::trunc);
    out << content;
}
}  // namespace

// Phase 4 of docs/NOTES_DESIGN.md: `kg dbdiff` learns a `note`/`note_problem`
// category. Two DATABASE COPIES synced from the SAME notes dir must produce
// an EMPTY note diff (notes are keyed by serialized_name, not by sqlite id
// or filesystem path -- see Snapshot.h's Phase 4 comment) even though the
// two DBs are otherwise independent sqlite files; a later edit that makes
// one of them stale must show up ONLY in the note category.
TEST_CASE("snapshot: dbdiff --only note is empty for two DB copies synced from the same notes dir, sees staleness", "[fixture][snapshot]")
{
    namespace fs = std::filesystem;
    const fs::path tmpRoot = fs::temp_directory_path() / fs::path("kg_dbdiff_notes_test");
    fs::remove_all(tmpRoot);
    fs::create_directories(tmpRoot);

    const std::string dbA = (tmpRoot / "a.srctrldb").string();
    const std::string dbB = (tmpRoot / "b.srctrldb").string();
    fs::copy_file(fixtureDbPath(), dbA);
    fs::copy_file(fixtureDbPath(), dbB);
    const std::string notesDir = (tmpRoot / "notes").string();
    fs::create_directories(notesDir);

    // Scaffold every FUNCTION/METHOD's placeholder block from dbA's own
    // symbol table (both dbA/dbB were copied from the identical fixture, so
    // their symbol tables -- and hence the scaffolded notes -- agree).
    {
        kg::db::Database db(dbA);
        const std::vector<kg::notes::SymbolRow> rows = kg::notes::listSymbols(db, 0, {});
        REQUIRE_FALSE(rows.empty());
        kg::notes::ScaffoldOptions opts;
        opts.notesDir = notesDir;
        kg::notes::scaffoldNotes(db, rows, opts);
    }

    // Fill in one block's brief so `filled` differs meaningfully between an
    // unfilled and a filled note.
    bool filledOne = false;
    for (const auto& entry : fs::recursive_directory_iterator(notesDir))
    {
        if (filledOne || entry.path().extension() != ".md")
        {
            continue;
        }
        std::string content = readFileText(entry.path().string());
        const std::string::size_type pos = content.find("_TODO_");
        if (pos != std::string::npos)
        {
            content.replace(pos, std::string("_TODO_").size(), "A deterministic brief.");
            writeFileText(entry.path().string(), content);
            filledOne = true;
        }
    }
    REQUIRE(filledOne);

    kg::notes::syncNotes(dbA, notesDir, false);
    kg::notes::syncNotes(dbB, notesDir, false);

    const kg::update::Snapshot snapA = takeSnapshot(dbA);
    const kg::update::Snapshot snapB = takeSnapshot(dbB);
    REQUIRE_FALSE(snapA.notes.empty());

    const kg::update::SnapshotDiff onlyNoteDiff = diffSnapshots(snapA, snapB, {"note"});
    REQUIRE(onlyNoteDiff.onlyInA.empty());
    REQUIRE(onlyNoteDiff.onlyInB.empty());

    // Now make dbA's cache disagree: hand-edit the `kg-hash` anchor on the
    // filled block to something wrong, so re-syncing dbA (only) reports it
    // stale while dbB's cache still says fresh.
    bool rewroteHash = false;
    for (const auto& entry : fs::recursive_directory_iterator(notesDir))
    {
        if (rewroteHash || entry.path().extension() != ".md")
        {
            continue;
        }
        std::string content = readFileText(entry.path().string());
        const std::string::size_type pos = content.find("kg-hash: ");
        if (pos != std::string::npos)
        {
            const std::string::size_type hashStart = pos + std::string("kg-hash: ").size();
            content.replace(hashStart, 16, "0000000000000000");
            writeFileText(entry.path().string(), content);
            rewroteHash = true;
        }
    }
    REQUIRE(rewroteHash);

    kg::notes::syncNotes(dbA, notesDir, false);
    // dbB is deliberately NOT re-synced -- it still reflects the old (fresh)
    // markdown it last saw, exactly like a worktree that hasn't pulled the
    // edit yet.

    const kg::update::Snapshot snapA2 = takeSnapshot(dbA);
    const kg::update::SnapshotDiff staleDiff = diffSnapshots(snapA2, snapB, {"note"});
    REQUIRE_FALSE(staleDiff.onlyInA.empty());
    REQUIRE_FALSE(staleDiff.onlyInB.empty());

    // Every other category is completely untouched by this whole exercise
    // (dbA/dbB's node/edge/etc rows never changed) -- proves --only note
    // really does restrict the diff to just the note category.
    const kg::update::SnapshotDiff fullDiff = diffSnapshots(snapA2, snapB);
    std::size_t nonNoteEntries = 0;
    for (const std::string& s : fullDiff.onlyInA)
    {
        if (s.rfind("note|", 0) != 0)
        {
            ++nonNoteEntries;
        }
    }
    for (const std::string& s : fullDiff.onlyInB)
    {
        if (s.rfind("note|", 0) != 0)
        {
            ++nonNoteEntries;
        }
    }
    REQUIRE(nonNoteEntries == 0);

    fs::remove_all(tmpRoot);
}
