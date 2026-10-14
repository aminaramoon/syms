// Phase 1 (incremental-update groundwork) assertions against the real
// synthetic fixture database (build/fixture.srctrldb -- see
// test/CMakeLists.txt's FIXTURES_REQUIRED FixtureDb wiring).

#include <catch2/catch_test_macros.hpp>

#include <cctype>
#include <set>
#include <string>

#include <sqlite3.h>

#include "symhow/db/Database.h"
#include "symhow/index/Provenance.h"

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

}  // namespace

TEST_CASE("incremental: provenance is present and looks right on a freshly-indexed fixture db", "[fixture]")
{
    symhow::db::Database db(fixtureDbPath());
    const auto prov = symhow::index::readProvenance(db.handle());
    REQUIRE(prov.has_value());
    REQUIRE(prov->walkerVersion == symhow::index::kWalkerVersion);
    REQUIRE_FALSE(prov->cdbPath.empty());
    REQUIRE(prov->cdbPath.front() == '/');  // absolute
    REQUIRE_FALSE(prov->indexedAtUtc.empty());
}

TEST_CASE("incremental: symhow_tu has one row per distinct TU PATH, each with a 16-hex-char args_hash", "[fixture]")
{
    symhow::db::Database db(fixtureDbPath());

    int tuCount = 0;
    db.run("SELECT count(*) FROM symhow_tu", [&](sqlite3_stmt* stmt) { tuCount = sqlite3_column_int(stmt, 0); });
    // The synthetic fixture project has 7 distinct TU source files (see
    // test_query_fixture.cpp's own assumptions about ns/overloads/chain/
    // templates/tu_a/tu_b.cpp, plus cfile.c -- a plain C TU added to
    // exercise the language-aware -std fallback) -- but 8 CDB ENTRIES:
    // templates.cpp is compiled twice, by two targets with different flags
    // (fixture_lib and fixture_lib_dup, see fixtures/synthetic/
    // CMakeLists.txt), exercising the duplicate-CDB-entry path
    // (symhow::index::pathArgsHashes()) that symhow_tu -- keyed by PATH, not by
    // (path, entry) -- has to collapse down to one row. symhow_tu row count
    // tracks distinct paths (7), not CDB entries (8).
    REQUIRE(tuCount == 7);

    db.run("SELECT args_hash FROM symhow_tu", [&](sqlite3_stmt* stmt) {
        const std::string hash = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 0));
        REQUIRE(hash.size() == 16);
        for (char c : hash)
        {
            REQUIRE(std::isxdigit(static_cast<unsigned char>(c)));
        }
    });
}

TEST_CASE("incremental: symhow_tu.entry_count reflects the real CDB entry count per path (templates.cpp is 2, everything else is 1)", "[fixture]")
{
    // Regression coverage for the duplicate-CDB-entry fix: templates.cpp is
    // compiled twice (see the TEST_CASE above), so its symhow_tu row must carry
    // entry_count=2; every other (non-duplicated) path must carry the
    // legacy-compatible default of 1.
    symhow::db::Database db(fixtureDbPath());

    int templatesEntryCount = -1;
    bool foundTemplates = false;
    db.run("SELECT path, entry_count FROM symhow_tu", [&](sqlite3_stmt* stmt) {
        const std::string path = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 0));
        const int entryCount = sqlite3_column_int(stmt, 1);
        if (path.size() >= 13 && path.compare(path.size() - 13, 13, "templates.cpp") == 0)
        {
            templatesEntryCount = entryCount;
            foundTemplates = true;
        }
        else
        {
            REQUIRE(entryCount == 1);
        }
    });
    REQUIRE(foundTemplates);
    REQUIRE(templatesEntryCount == 2);
}

TEST_CASE("incremental: every symhow_tu row from a fresh index has status='ok'", "[fixture]")
{
    // Phase 4: symhow_tu.status is the precise per-TU outcome tracked instead of
    // the old (Phase 3) "no outgoing INCLUDE edge" heuristic. The synthetic
    // fixture is a clean, fully-parseable corpus with no fatal diagnostics
    // and no CUDA rungs at all, so every row must be "ok".
    symhow::db::Database db(fixtureDbPath());

    int tuCount = 0;
    db.run("SELECT count(*) FROM symhow_tu", [&](sqlite3_stmt* stmt) { tuCount = sqlite3_column_int(stmt, 0); });
    REQUIRE(tuCount == 7);

    int notOkCount = 0;
    db.run("SELECT count(*) FROM symhow_tu WHERE status != 'ok'", [&](sqlite3_stmt* stmt) { notOkCount = sqlite3_column_int(stmt, 0); });
    REQUIRE(notOkCount == 0);
}

TEST_CASE("incremental: symhow_symbol_def is non-empty and level0's declaration points at chain.cpp", "[fixture]")
{
    symhow::db::Database db(fixtureDbPath());

    int defCount = 0;
    db.run("SELECT count(*) FROM symhow_symbol_def", [&](sqlite3_stmt* stmt) { defCount = sqlite3_column_int(stmt, 0); });
    REQUIRE(defCount > 0);

    std::set<std::string> filesForLevel0;
    db.run(
        "SELECT f.path FROM node n "
        "JOIN symhow_symbol_def sd ON sd.node_id = n.id "
        "JOIN file f ON f.id = sd.file_node_id "
        "WHERE n.serialized_name LIKE '%level0%'",
        [&](sqlite3_stmt* stmt) { filesForLevel0.insert(reinterpret_cast<const char*>(sqlite3_column_text(stmt, 0))); });
    REQUIRE_FALSE(filesForLevel0.empty());
    bool foundChainCpp = false;
    for (const auto& path : filesForLevel0)
    {
        if (path.size() >= 9 && path.compare(path.size() - 9, 9, "chain.cpp") == 0)
        {
            foundChainCpp = true;
        }
    }
    REQUIRE(foundChainCpp);
}

TEST_CASE("incremental: every non-MEMBER edge has at least one occurrence", "[fixture]")
{
    symhow::db::Database db(fixtureDbPath());
    int badEdges = -1;
    db.run(
        "SELECT COUNT(*) FROM edge e WHERE e.type<>1 AND NOT EXISTS "
        "(SELECT 1 FROM occurrence o WHERE o.element_id = e.id)",
        [&](sqlite3_stmt* stmt) { badEdges = sqlite3_column_int(stmt, 0); });
    REQUIRE(badEdges == 0);
}

TEST_CASE("incremental: no source_location references a file_node_id missing from node", "[fixture]")
{
    symhow::db::Database db(fixtureDbPath());
    int orphans = -1;
    db.run(
        "SELECT COUNT(*) FROM source_location sl WHERE sl.file_node_id NOT IN (SELECT id FROM node)",
        [&](sqlite3_stmt* stmt) { orphans = sqlite3_column_int(stmt, 0); });
    REQUIRE(orphans == 0);
}

// --- node.type upgrade-only regression coverage (BLOCKER finding #1) -------
//
// Before the fix, unknownNodeKindInt/typeNodeKindInt were both computed via
// translateSymbolKind(), which maps Unknown AND Type to
// sourcetrail::SymbolKind::TYPE -- so the CASE's THEN 0 / THEN 1 arms both
// tested for the same int, every freshly-created node (always
// NodeKind::UNKNOWN = 1, see addNodeHierarchy() in the vendored writer)
// fell into the ELSE arm, and was never upgraded to TYPE (=2). These three
// cases pin the fix: no symbol-backed node should be left at type=1
// (UNKNOWN), the only type=1 node left should be the vendored writer's
// single shared "unsolved symbol" node, and at least one node should have
// been upgraded to type=2 (TYPE) -- template type parameters and the like.

TEST_CASE("incremental: no symbol-backed node is left at node.type = UNKNOWN", "[fixture]")
{
    symhow::db::Database db(fixtureDbPath());
    int badNodes = -1;
    db.run(
        "SELECT COUNT(*) FROM node n JOIN symbol s ON s.id = n.id WHERE n.type = 1",
        [&](sqlite3_stmt* stmt) { badNodes = sqlite3_column_int(stmt, 0); });
    REQUIRE(badNodes == 0);
}

TEST_CASE("incremental: exactly one node.type = UNKNOWN remains (the shared unsolved-symbol node)", "[fixture]")
{
    symhow::db::Database db(fixtureDbPath());
    int unknownCount = -1;
    db.run(
        "SELECT COUNT(*) FROM node WHERE type = 1", [&](sqlite3_stmt* stmt) { unknownCount = sqlite3_column_int(stmt, 0); });
    REQUIRE(unknownCount == 1);
}

TEST_CASE("incremental: at least one node was upgraded to node.type = TYPE", "[fixture]")
{
    symhow::db::Database db(fixtureDbPath());
    int typeCount = -1;
    db.run("SELECT COUNT(*) FROM node WHERE type = 2", [&](sqlite3_stmt* stmt) { typeCount = sqlite3_column_int(stmt, 0); });
    REQUIRE(typeCount > 0);
}

// --- symhow_tu.file_node_id must always resolve (path-normalization, #2) ------

TEST_CASE("incremental: every symhow_tu row has a resolved file_node_id", "[fixture]")
{
    symhow::db::Database db(fixtureDbPath());
    int nullCount = -1;
    db.run(
        "SELECT COUNT(*) FROM symhow_tu WHERE file_node_id IS NULL",
        [&](sqlite3_stmt* stmt) { nullCount = sqlite3_column_int(stmt, 0); });
    REQUIRE(nullCount == 0);
}

// --- definition_kind upgrade-only, and a header-decl/cpp-def symbol (#5) --

TEST_CASE("incremental: every symbol row's definition_kind is Explicit (2) in this fixture", "[fixture]")
{
    // The synthetic fixture only exercises symbols that get an explicit
    // definition somewhere -- definition_kind should never regress below
    // Explicit (2) for any symbol row visited by this fixture's TUs.
    symhow::db::Database db(fixtureDbPath());
    int belowExplicit = -1;
    db.run(
        "SELECT COUNT(*) FROM symbol WHERE definition_kind < 2",
        [&](sqlite3_stmt* stmt) { belowExplicit = sqlite3_column_int(stmt, 0); });
    REQUIRE(belowExplicit == 0);
}

TEST_CASE("incremental: a header-declared, cpp-defined function has definition_kind Explicit and two symhow_symbol_def rows", "[fixture]")
{
    // fixture::squarePerimeter is declared in include/shapes.hpp and defined
    // out-of-line in src/ns.cpp (see both files' comments) -- this pins
    // down that (a) the out-of-line definition wins definition_kind =
    // Explicit (2), not just "declared", and (b) symhow_symbol_def has one row
    // per file the symbol was declared/defined in (header AND cpp), not
    // just the last one processed.
    symhow::db::Database db(fixtureDbPath());

    int definitionKind = -1;
    bool found = false;
    db.run(
        "SELECT s.definition_kind FROM node n JOIN symbol s ON s.id = n.id "
        "WHERE n.serialized_name LIKE '%squarePerimeter%'",
        [&](sqlite3_stmt* stmt) {
            definitionKind = sqlite3_column_int(stmt, 0);
            found = true;
        });
    REQUIRE(found);
    REQUIRE(definitionKind == 2);  // Explicit

    int defFileCount = -1;
    db.run(
        "SELECT COUNT(*) FROM symhow_symbol_def sd "
        "JOIN node n ON n.id = sd.node_id "
        "WHERE n.serialized_name LIKE '%squarePerimeter%'",
        [&](sqlite3_stmt* stmt) { defFileCount = sqlite3_column_int(stmt, 0); });
    REQUIRE(defFileCount == 2);

    std::set<std::string> declFiles;
    db.run(
        "SELECT f.path FROM symhow_symbol_def sd "
        "JOIN node n ON n.id = sd.node_id "
        "JOIN file f ON f.id = sd.file_node_id "
        "WHERE n.serialized_name LIKE '%squarePerimeter%'",
        [&](sqlite3_stmt* stmt) { declFiles.insert(reinterpret_cast<const char*>(sqlite3_column_text(stmt, 0))); });
    REQUIRE(declFiles.size() == 2);

    bool foundHeader = false;
    bool foundCpp = false;
    for (const auto& path : declFiles)
    {
        if (path.size() >= 10 && path.compare(path.size() - 10, 10, "shapes.hpp") == 0)
        {
            foundHeader = true;
        }
        if (path.size() >= 6 && path.compare(path.size() - 6, 6, "ns.cpp") == 0)
        {
            foundCpp = true;
        }
    }
    REQUIRE(foundHeader);
    REQUIRE(foundCpp);
}

// --- Phase 5: every GC rule Invalidator applies must be a provable no-op
// on a freshly indexed database (one that was never invalidated). Each of
// the TEST_CASEs below runs the EXACT predicate the corresponding rule in
// src/update/Invalidator.cpp deletes by, and requires it to match 0 rows.
// The same three assertions run against the real cucs corpus in
// test/update/cucs_update_scenario.cmake.

TEST_CASE("incremental: a fresh index's symhow_touched covers every node (GC guard is total)", "[fixture]")
{
    symhow::db::Database db(fixtureDbPath());
    int untouched = -1;
    db.run(
        "SELECT COUNT(*) FROM node n WHERE n.id NOT IN (SELECT node_id FROM symhow_touched)",
        [&](sqlite3_stmt* stmt) { untouched = sqlite3_column_int(stmt, 0); });
    // SrctrldbBackend::finish() writes symhow_touched from the ids THIS pass
    // created or looked up; for a full `symhow index` that is every node in the
    // database. This is what makes Invalidator's `symhow_touched` guard safe:
    // after a full index nothing is deletable, after a partial re-index
    // exactly the nodes the re-indexed TUs no longer produce are.
    REQUIRE(untouched == 0);
}

TEST_CASE("incremental: Invalidator's dangling-edge rule matches 0 rows on a fresh index", "[fixture]")
{
    symhow::db::Database db(fixtureDbPath());
    int rows = -1;
    db.run(
        "SELECT COUNT(*) FROM edge e WHERE e.type<>1 AND e.source_node_id NOT IN (SELECT id FROM file) "
        "AND e.source_node_id NOT IN (SELECT node_id FROM symhow_touched) "
        "AND NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.element_id=e.source_node_id)",
        [&](sqlite3_stmt* stmt) { rows = sqlite3_column_int(stmt, 0); });
    REQUIRE(rows == 0);
}

TEST_CASE("incremental: Invalidator's dead-node rule matches 0 rows on a fresh index", "[fixture]")
{
    symhow::db::Database db(fixtureDbPath());
    int rows = -1;
    db.run(
        "SELECT COUNT(*) FROM node n WHERE n.id NOT IN (SELECT id FROM file) "
        "AND n.id NOT IN (SELECT node_id FROM symhow_touched) "
        "AND NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.element_id=n.id) "
        "AND NOT EXISTS (SELECT 1 FROM edge e WHERE e.type<>1 AND (e.source_node_id=n.id OR e.target_node_id=n.id)) "
        "AND NOT EXISTS (SELECT 1 FROM edge e WHERE e.type=1 AND e.source_node_id=n.id)",
        [&](sqlite3_stmt* stmt) { rows = sqlite3_column_int(stmt, 0); });
    REQUIRE(rows == 0);
}

TEST_CASE("incremental: no non-error occurrence location lands in a non-indexed file", "[fixture]")
{
    symhow::db::Database db(fixtureDbPath());
    int rows = -1;
    // AstWalker::rangeFileIndexed(): a fact is only ever emitted if ITS OWN
    // range resolves to an indexed file. `source_location.type = 6` is
    // Sourcetrail's INDEXER_ERROR kind, which legitimately attaches to the
    // TU's own main file even when the diagnostic came from elsewhere.
    db.run(
        "SELECT COUNT(*) FROM source_location l JOIN file f ON f.id = l.file_node_id "
        "WHERE f.indexed = 0 AND l.type <> 6",
        [&](sqlite3_stmt* stmt) { rows = sqlite3_column_int(stmt, 0); });
    REQUIRE(rows == 0);
}
