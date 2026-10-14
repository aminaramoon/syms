// Phase 1 smoke test for the vendored SourcetrailDB writer
// (third_party/sourcetraildb, target symhow_sourcetraildb).
//
// This is the real deliverable proving the vendored writer works end to
// end: it writes a small symbol hierarchy (a namespace containing two
// functions, one of which calls the other) to a fresh .srctrldb file using
// sourcetrail::SourcetrailDBWriter, closes it, then reopens the file with a
// raw sqlite3 connection and asserts on the schema and on the exact
// node/edge/element/symbol counts that recording should have produced.

#include <catch2/catch_test_macros.hpp>

#include <sqlite3.h>

#include <cstdio>
#include <filesystem>
#include <set>
#include <string>

#include "DefinitionKind.h"
#include "NameHierarchy.h"
#include "SourceRange.h"
#include "SourcetrailDBWriter.h"
#include "SymbolKind.h"
#include "ReferenceKind.h"

namespace
{

std::filesystem::path testDatabasePath()
{
#ifdef SYMHOW_TEST_BINARY_DIR
    std::filesystem::path dir(SYMHOW_TEST_BINARY_DIR);
#else
    std::filesystem::path dir = std::filesystem::temp_directory_path();
#endif
    std::filesystem::create_directories(dir);
    return dir / "writer_smoke.srctrldb";
}

// Runs `query` against `db`, expecting exactly one row with one TEXT/INTEGER
// column, and returns it as a string. Fails the current Catch2 assertion on
// any sqlite error.
std::string queryScalar(sqlite3* db, const std::string& query)
{
    sqlite3_stmt* stmt = nullptr;
    REQUIRE(sqlite3_prepare_v2(db, query.c_str(), -1, &stmt, nullptr) == SQLITE_OK);
    REQUIRE(sqlite3_step(stmt) == SQLITE_ROW);
    const unsigned char* text = sqlite3_column_text(stmt, 0);
    std::string result = text ? reinterpret_cast<const char*>(text) : "";
    sqlite3_finalize(stmt);
    return result;
}

int queryCount(sqlite3* db, const std::string& table)
{
    return std::stoi(queryScalar(db, "SELECT COUNT(*) FROM " + table + ";"));
}

}  // namespace

TEST_CASE("SourcetrailDBWriter records a symbol hierarchy that survives a round trip", "[writer][smoke]")
{
    const std::filesystem::path dbPath = testDatabasePath();
    std::filesystem::remove(dbPath);
    std::filesystem::remove(dbPath.string() + "-journal");
    // open() also creates a sibling .srctrlprj project file; clean that up too.
    std::filesystem::path projectPath = dbPath;
    projectPath.replace_extension(".srctrlprj");
    std::filesystem::remove(projectPath);

    int fooId = 0;
    int barId = 0;
    int namespaceId = 0;
    int referenceId = 0;

    // --- Write phase ---------------------------------------------------
    {
        sourcetrail::SourcetrailDBWriter writer;
        REQUIRE(writer.getSupportedDatabaseVersion() == 25);
        REQUIRE(writer.getVersionString() == "v4.db25.p1");

        REQUIRE(writer.open(dbPath.string()));
        REQUIRE(writer.isEmpty());
        REQUIRE(writer.isCompatible());

        REQUIRE(writer.beginTransaction());

        // A file to attach locations to. It doesn't need to exist on disk;
        // SourcetrailDB tolerates that (file content is simply left empty).
        const int fileId = writer.recordFile("/nonexistent/example.cpp");
        REQUIRE(fileId != 0);
        REQUIRE(writer.recordFileLanguage(fileId, "cpp"));

        // namespace myns
        sourcetrail::NameHierarchy namespaceHierarchy;
        namespaceHierarchy.nameDelimiter = "::";
        namespaceHierarchy.nameElements.push_back({"", "myns", ""});
        namespaceId = writer.recordSymbol(namespaceHierarchy);
        REQUIRE(namespaceId != 0);
        REQUIRE(writer.recordSymbolKind(namespaceId, sourcetrail::SymbolKind::NAMESPACE));
        REQUIRE(writer.recordSymbolDefinitionKind(namespaceId, sourcetrail::DefinitionKind::EXPLICIT));

        // void myns::foo()
        sourcetrail::NameHierarchy fooHierarchy;
        fooHierarchy.nameDelimiter = "::";
        fooHierarchy.nameElements.push_back({"", "myns", ""});
        fooHierarchy.nameElements.push_back({"void ", "foo", "()"});
        fooId = writer.recordSymbol(fooHierarchy);
        REQUIRE(fooId != 0);
        REQUIRE(writer.recordSymbolKind(fooId, sourcetrail::SymbolKind::FUNCTION));
        REQUIRE(writer.recordSymbolDefinitionKind(fooId, sourcetrail::DefinitionKind::EXPLICIT));
        REQUIRE(writer.recordSymbolLocation(fooId, sourcetrail::SourceRange{fileId, 3, 1, 3, 10}));

        // void myns::bar(), which calls foo()
        sourcetrail::NameHierarchy barHierarchy;
        barHierarchy.nameDelimiter = "::";
        barHierarchy.nameElements.push_back({"", "myns", ""});
        barHierarchy.nameElements.push_back({"void ", "bar", "()"});
        barId = writer.recordSymbol(barHierarchy);
        REQUIRE(barId != 0);
        REQUIRE(writer.recordSymbolKind(barId, sourcetrail::SymbolKind::FUNCTION));
        REQUIRE(writer.recordSymbolDefinitionKind(barId, sourcetrail::DefinitionKind::EXPLICIT));
        REQUIRE(writer.recordSymbolLocation(barId, sourcetrail::SourceRange{fileId, 6, 1, 6, 10}));

        // bar() -> foo()
        referenceId = writer.recordReference(barId, fooId, sourcetrail::ReferenceKind::CALL);
        REQUIRE(referenceId != 0);
        REQUIRE(writer.recordReferenceLocation(referenceId, sourcetrail::SourceRange{fileId, 7, 5, 7, 10}));

        REQUIRE(writer.commitTransaction());
        // Note: SourcetrailDBWriter::isEmpty() (DatabaseStorage::isEmpty())
        // checks only whether the "meta" table exists in sqlite_master, not
        // whether any data rows have been written to it. Since open()
        // unconditionally creates the "meta" table via setupDatabaseTables(),
        // isEmpty() is true for the whole lifetime of a writer after open()
        // regardless of how much data has been recorded -- so it's not
        // asserted again here after writing/committing data. The real
        // "is there data" check is the read-back phase below.
        REQUIRE(writer.close());
        REQUIRE(writer.getLastError().empty());
    }

    // The database file (and its sibling project file) must now exist on disk.
    REQUIRE(std::filesystem::exists(dbPath));

    // --- Read-back phase: reopen via a raw sqlite3 connection ----------
    sqlite3* db = nullptr;
    REQUIRE(sqlite3_open_v2(dbPath.string().c_str(), &db, SQLITE_OPEN_READWRITE, nullptr) == SQLITE_OK);

    SECTION("schema version matches SourcetrailDB v4.db25.p1")
    {
        CHECK(queryScalar(db, "SELECT value FROM meta WHERE key = 'storage_version';") == "25");
    }

    SECTION("all 13 expected Sourcetrail-schema tables exist")
    {
        static const std::set<std::string> expectedTables = {
            "meta",
            "element",
            "element_component",
            "edge",
            "node",
            "symbol",
            "file",
            "filecontent",
            "local_symbol",
            "source_location",
            "occurrence",
            "component_access",
            "error",
        };

        sqlite3_stmt* stmt = nullptr;
        REQUIRE(
            sqlite3_prepare_v2(
                db, "SELECT name FROM sqlite_master WHERE type = 'table';", -1, &stmt, nullptr) ==
            SQLITE_OK);

        std::set<std::string> actualTables;
        while (sqlite3_step(stmt) == SQLITE_ROW)
        {
            const unsigned char* name = sqlite3_column_text(stmt, 0);
            if (name)
            {
                actualTables.insert(reinterpret_cast<const char*>(name));
            }
        }
        sqlite3_finalize(stmt);

        for (const auto& table : expectedTables)
        {
            INFO("expected table: " << table);
            CHECK(actualTables.count(table) == 1);
        }
    }

    SECTION("recorded node/edge/element/symbol counts match what was written")
    {
        // Nodes: myns, myns::foo, myns::bar, and the file node = 4.
        CHECK(queryCount(db, "node") == 4);

        // Edges: MEMBER(myns -> foo), MEMBER(myns -> bar) implicitly created
        // by recordSymbol()'s hierarchy walk, plus the explicit CALL(bar ->
        // foo) edge from recordReference() = 3.
        CHECK(queryCount(db, "edge") == 3);

        // Every node and edge occupies exactly one row in the shared
        // "element" table (4 nodes + 3 edges).
        CHECK(queryCount(db, "element") == 7);

        // Symbol definition-kind rows: namespace, foo, bar = 3.
        CHECK(queryCount(db, "symbol") == 3);

        // Source locations: foo's location, bar's location, and the CALL
        // reference's location = 3, each with a corresponding occurrence.
        CHECK(queryCount(db, "source_location") == 3);
        CHECK(queryCount(db, "occurrence") == 3);

        CHECK(queryCount(db, "file") == 1);
    }

    sqlite3_close(db);
}
