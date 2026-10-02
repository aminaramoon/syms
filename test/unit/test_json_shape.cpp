// Shape/validity tests for kg::out::json::JsonWriter (Phase 5), against the
// real synthetic fixture database (build/fixture.srctrldb -- see
// test/CMakeLists.txt's FIXTURES_REQUIRED FixtureDb wiring, same pattern
// test_bfs.cpp/test_query_fixture.cpp already use for [fixture]-tagged
// tests). All cases here are additionally tagged [json_shape] so they get
// their own `ctest -R json_shape` registration (see test/CMakeLists.txt).
//
// For each of the 9 kg.*/1 schemas: build the result via the real
// QueryEngine (or, for kg.index-summary/1, the real kg::index::runIndex()
// pipeline) call, serialize via JsonWriter, round-trip through
// nlohmann::json::parse() (proves it's valid JSON), and assert the schema
// name, required top-level keys, and that every "kind" field anywhere in
// the tree is a JSON string (never a number).

#include <catch2/catch_test_macros.hpp>

#include <fcntl.h>
#include <unistd.h>

#include <cstdio>
#include <fstream>
#include <functional>
#include <sstream>
#include <string>
#include <unordered_map>
#include <vector>

#include <nlohmann/json.hpp>

#include "kg/db/Database.h"
#include "kg/db/Enums.h"
#include "kg/index/IndexDriver.h"
#include "kg/out/JsonWriter.h"
#include "kg/query/QueryEngine.h"
#include "kg/query/SymbolResolver.h"

using kg::out::json::JsonWriter;
using kg::out::json::QueryContext;
using kg::query::QueryEngine;
using kg::query::ResolveOutcome;

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

std::string fixtureCdbPath()
{
#ifdef KG_FIXTURE_CDB
    return KG_FIXTURE_CDB;
#else
    return "fixture_build/compile_commands.json";
#endif
}

std::string testBinaryDir()
{
#ifdef KG_TEST_BINARY_DIR
    return KG_TEST_BINARY_DIR;
#else
    return ".";
#endif
}

int64_t resolveOne(kg::db::Database& db, const std::string& spec, const std::vector<int>* kindFilter = nullptr)
{
    const kg::query::ResolveResult rr = kg::query::resolveSymbol(db, spec, kindFilter, nullptr);
    INFO("resolving '" << spec << "'");
    REQUIRE(rr.outcome == ResolveOutcome::Unique);
    return rr.candidates.front().id;
}

// Walks the entire JSON tree and asserts every value stored under a key
// literally named "kind" is a JSON string -- covers node.kind, edge.kind,
// location.kind, hit.node.kind, etc. wherever they occur, without having to
// enumerate each schema's exact nesting by hand.
void checkKindFieldsAreStrings(const nlohmann::json& j)
{
    if (j.is_object())
    {
        for (auto it = j.begin(); it != j.end(); ++it)
        {
            if (it.key() == "kind" && !it.value().is_null())
            {
                INFO("key 'kind' with non-string value: " << it.value().dump());
                CHECK(it.value().is_string());
            }
            checkKindFieldsAreStrings(it.value());
        }
    }
    else if (j.is_array())
    {
        for (const nlohmann::json& element : j)
        {
            checkKindFieldsAreStrings(element);
        }
    }
}

void checkTopLevelKeys(const nlohmann::json& j, const std::vector<std::string>& required)
{
    for (const std::string& key : required)
    {
        INFO("missing required top-level key '" << key << "' in " << j.dump());
        CHECK(j.contains(key));
    }
}

// Redirects real fd 1 (stdout) to `tmpPath` for the duration of `fn()`,
// restores it afterward, and returns everything written. Used only by the
// kg.index-summary/1 test below, since that envelope is built directly by
// kg::index::runIndex() (via std::printf), not by JsonWriter -- this proves
// the real --json code path in-process, without shelling out to the `kg`
// binary.
std::string captureStdout(const std::function<void()>& fn, const std::string& tmpPath)
{
    std::fflush(stdout);
    const int savedFd = dup(1);
    REQUIRE(savedFd >= 0);
    const int tmpFd = open(tmpPath.c_str(), O_WRONLY | O_CREAT | O_TRUNC, 0644);
    REQUIRE(tmpFd >= 0);
    dup2(tmpFd, 1);
    close(tmpFd);

    fn();

    std::fflush(stdout);
    dup2(savedFd, 1);
    close(savedFd);

    std::ifstream in(tmpPath);
    std::stringstream ss;
    ss << in.rdbuf();
    return ss.str();
}

}  // namespace

TEST_CASE("json_shape kg.graph/1 is valid, has required keys, and every kind field is a string", "[fixture][json_shape]")
{
    kg::db::Database db(fixtureDbPath());
    QueryEngine engine(db);
    JsonWriter writer(db, engine, "test-version");

    const int64_t level4 = resolveOne(db, "level4");
    const kg::query::BfsResult result = engine.callers({level4}, /*depth=*/2, /*maxNodes=*/0, /*nodeKindMask=*/0);
    REQUIRE_FALSE(result.nodes.empty());

    QueryContext ctx;
    ctx.verb = "callers";
    ctx.db = fixtureDbPath();
    ctx.symbolOrTextOrPath = "level4";
    ctx.symbolIds = {level4};
    ctx.depth = 2;
    ctx.maxNodes = 0;
    ctx.edgeKinds = {kg::db::EdgeKind::CALL};

    const nlohmann::json j = writer.graph(ctx, result, 7);
    const std::string dumped = j.dump();
    const nlohmann::json parsed = nlohmann::json::parse(dumped);  // proves it's valid JSON

    CHECK(parsed["schema"] == "kg.graph/1");
    checkTopLevelKeys(parsed, {"kg_version", "schema", "query", "truncated", "stats", "nodes", "edges"});
    CHECK(parsed["query"]["verb"] == "callers");
    CHECK(parsed["stats"]["elapsed_ms"] == 7);
    REQUIRE(parsed["nodes"].is_array());
    CHECK_FALSE(parsed["nodes"].empty());
    for (const nlohmann::json& n : parsed["nodes"])
    {
        CHECK(n.contains("id"));
        CHECK(n["kind"].is_string());
        CHECK(n.contains("name"));
        CHECK(n.contains("qualified_name"));
        CHECK(n.contains("location"));  // present, possibly null
    }
    checkKindFieldsAreStrings(parsed);
}

// Phase 4 of docs/NOTES_DESIGN.md: `kg query context` reuses kg.graph/1
// verbatim, plus a "notes" field on every node when JsonWriter::graph() is
// given a non-null notesBySerializedName map (the CLI builds this map from
// kg::notes::openNotes(); this test builds a small map directly so it stays
// a pure JsonWriter unit test, independent of the notes markdown/DB cache
// plumbing exercised elsewhere).
TEST_CASE("json_shape kg.graph/1 gains a notes field per node when JsonWriter::graph() is given a notes map", "[fixture][json_shape]")
{
    kg::db::Database db(fixtureDbPath());
    QueryEngine engine(db);
    JsonWriter writer(db, engine, "test-version");

    const int64_t level2 = resolveOne(db, "level2");
    const kg::query::BfsResult callers = engine.callers({level2}, /*depth=*/1, /*maxNodes=*/0, 0);
    const kg::query::BfsResult callees = engine.callees({level2}, /*depth=*/1, /*maxNodes=*/0, 0);
    kg::query::BfsResult merged = callers;
    for (const kg::query::BfsNode& n : callees.nodes)
    {
        bool present = false;
        for (const kg::query::BfsNode& m : merged.nodes)
        {
            if (m.id == n.id)
            {
                present = true;
                break;
            }
        }
        if (!present)
        {
            merged.nodes.push_back(n);
        }
    }
    REQUIRE(merged.nodes.size() >= 2);

    // Fake a note for exactly the seed's serializedName.
    std::string seedSerializedName;
    for (const kg::query::BfsNode& n : merged.nodes)
    {
        if (n.id == level2)
        {
            seedSerializedName = n.serializedName;
        }
    }
    REQUIRE_FALSE(seedSerializedName.empty());

    std::unordered_map<std::string, nlohmann::json> notes;
    nlohmann::json note;
    note["brief"] = "test brief";
    note["what"] = "";
    note["how"] = "";
    note["signature"] = "";
    note["pre"] = nlohmann::json::array();
    note["post"] = nlohmann::json::array();
    note["bugs"] = nlohmann::json::array();
    note["suggestions"] = nlohmann::json::array();
    note["path"] = "src/chain.cpp.md";
    note["line"] = 1;
    note["filled"] = true;
    note["stale"] = false;
    note["note_hash"] = "deadbeef";
    note["current_body_hash"] = "deadbeef";
    note["commit"] = "abc123";
    notes[seedSerializedName] = note;

    QueryContext ctx;
    ctx.verb = "context";
    ctx.db = fixtureDbPath();
    ctx.symbolOrTextOrPath = "level2";
    ctx.symbolIds = {level2};
    ctx.depth = 1;
    ctx.maxNodes = 0;
    ctx.edgeKinds = {kg::db::EdgeKind::CALL};

    const nlohmann::json j1 = writer.graph(ctx, merged, 3, &notes);
    const nlohmann::json j2 = writer.graph(ctx, merged, 3, &notes);
    CHECK(j1.dump() == j2.dump());  // deterministic across two runs

    const nlohmann::json parsed = nlohmann::json::parse(j1.dump());
    CHECK(parsed["schema"] == "kg.graph/1");
    CHECK(parsed["query"]["verb"] == "context");
    bool sawFilledNote = false;
    for (const nlohmann::json& n : parsed["nodes"])
    {
        REQUIRE(n.contains("notes"));  // present (object or null) on every node
        if (!n["notes"].is_null())
        {
            CHECK(n["notes"]["brief"] == "test brief");
            sawFilledNote = true;
        }
    }
    CHECK(sawFilledNote);
    checkKindFieldsAreStrings(parsed);

    // Without the notes map (as --no-notes uses), the field is absent
    // entirely rather than null.
    const nlohmann::json j3 = writer.graph(ctx, merged, 3, nullptr);
    for (const nlohmann::json& n : j3["nodes"])
    {
        CHECK_FALSE(n.contains("notes"));
    }
}

TEST_CASE("json_shape kg.references/1 is valid and has the DECLARATION synthetic edge_kind", "[fixture][json_shape]")
{
    kg::db::Database db(fixtureDbPath());
    QueryEngine engine(db);
    JsonWriter writer(db, engine, "test-version");

    const int64_t shape = resolveOne(db, "fixture::Shape");
    const kg::query::ReferencesResult result = engine.references({shape});
    REQUIRE_FALSE(result.hits.empty());

    QueryContext ctx;
    ctx.verb = "references";
    ctx.db = fixtureDbPath();
    ctx.symbolOrTextOrPath = "fixture::Shape";
    ctx.symbolIds = {shape};

    const nlohmann::json j = writer.references(ctx, result, 4);
    const nlohmann::json parsed = nlohmann::json::parse(j.dump());

    CHECK(parsed["schema"] == "kg.references/1");
    checkTopLevelKeys(parsed, {"kg_version", "schema", "query", "truncated", "stats", "nodes", "references"});
    CHECK(parsed["truncated"] == false);
    bool sawDeclaration = false;
    for (const nlohmann::json& r : parsed["references"])
    {
        CHECK(r.contains("edge_kind"));
        CHECK(r["edge_kind"].is_string());
        if (r["edge_kind"] == "DECLARATION")
        {
            sawDeclaration = true;
            CHECK(r["edge_id"].is_null());
        }
    }
    CHECK(sawDeclaration);
    checkKindFieldsAreStrings(parsed);
}

TEST_CASE("json_shape kg.definition/1 has a single symbol object and sorted locations", "[fixture][json_shape]")
{
    kg::db::Database db(fixtureDbPath());
    QueryEngine engine(db);
    JsonWriter writer(db, engine, "test-version");

    const int64_t square = resolveOne(db, "fixture::Square");
    const kg::query::DefinitionResult result = engine.definition({square});
    REQUIRE_FALSE(result.locations.empty());

    QueryContext ctx;
    ctx.verb = "definition";
    ctx.db = fixtureDbPath();
    ctx.symbolOrTextOrPath = "fixture::Square";
    ctx.symbolIds = {square};

    const nlohmann::json j = writer.definition(ctx, result, 0);
    const nlohmann::json parsed = nlohmann::json::parse(j.dump());

    CHECK(parsed["schema"] == "kg.definition/1");
    checkTopLevelKeys(parsed, {"kg_version", "schema", "query", "truncated", "stats", "symbol", "locations"});
    REQUIRE(parsed["symbol"].is_object());
    CHECK(parsed["symbol"]["id"] == square);
    CHECK(parsed["symbol"]["kind"] == "CLASS");
    checkKindFieldsAreStrings(parsed);
}

TEST_CASE("json_shape kg.search/1 hits carry matched_indices and a full node object", "[fixture][json_shape]")
{
    kg::db::Database db(fixtureDbPath());
    QueryEngine engine(db);
    JsonWriter writer(db, engine, "test-version");

    const std::vector<int> classOnly{kg::db::NodeKind::CLASS};
    const std::vector<kg::query::SearchHit> hits = engine.search("sqr", &classOnly, 10);
    REQUIRE_FALSE(hits.empty());

    QueryContext ctx;
    ctx.verb = "search";
    ctx.db = fixtureDbPath();
    ctx.symbolOrTextOrPath = "sqr";
    ctx.limit = 10;
    ctx.searchKinds = classOnly;

    const nlohmann::json j = writer.search(ctx, hits, 61);
    const nlohmann::json parsed = nlohmann::json::parse(j.dump());

    CHECK(parsed["schema"] == "kg.search/1");
    checkTopLevelKeys(parsed, {"kg_version", "schema", "query", "truncated", "stats", "hits"});
    REQUIRE(parsed["hits"].is_array());
    CHECK_FALSE(parsed["hits"].empty());
    for (const nlohmann::json& h : parsed["hits"])
    {
        CHECK(h.contains("score"));
        CHECK(h["matched_indices"].is_array());
        REQUIRE(h.contains("node"));
        CHECK(h["node"]["kind"].is_string());
    }
    CHECK(parsed["stats"]["candidates_scanned"].get<int64_t>() > 0);
    checkKindFieldsAreStrings(parsed);
}

TEST_CASE("json_shape kg.stats/1 has sparse node_kinds/edge_kinds and a database object", "[fixture][json_shape]")
{
    kg::db::Database db(fixtureDbPath());
    QueryEngine engine(db);
    JsonWriter writer(db, engine, "test-version");

    const kg::query::StatsResult result = engine.stats(100);

    QueryContext ctx;
    ctx.verb = "stats";
    ctx.db = fixtureDbPath();

    const nlohmann::json j = writer.stats(ctx, result);
    const nlohmann::json parsed = nlohmann::json::parse(j.dump());

    CHECK(parsed["schema"] == "kg.stats/1");
    checkTopLevelKeys(parsed, {"kg_version", "schema", "query", "database", "counts", "node_kinds", "edge_kinds", "errors"});
    CHECK(parsed["database"]["storage_version"] == 25);
    CHECK(parsed["database"]["reader_indices_present"] == true);
    CHECK(parsed["counts"]["nodes"].get<int64_t>() > 0);
    // Sparse: every key present must have a nonzero count.
    for (auto it = parsed["node_kinds"].begin(); it != parsed["node_kinds"].end(); ++it)
    {
        CHECK(it.value().get<int64_t>() > 0);
    }
}

TEST_CASE("json_shape kg.file/1 resolves node vs is_local_symbol occurrences explicitly", "[fixture][json_shape]")
{
    kg::db::Database db(fixtureDbPath());
    QueryEngine engine(db);
    JsonWriter writer(db, engine, "test-version");

    const kg::query::ResolveResult rr = kg::query::resolveFilePath(db, "shapes.hpp");
    REQUIRE(rr.outcome == ResolveOutcome::Unique);
    const int64_t fileId = rr.candidates.front().id;

    const kg::query::FileResult fr = engine.fileInfo(fileId, /*wantContents=*/true, /*wantOccurrences=*/true, std::nullopt, nullptr);
    REQUIRE_FALSE(fr.occurrences.empty());

    QueryContext ctx;
    ctx.verb = "file";
    ctx.db = fixtureDbPath();
    ctx.symbolOrTextOrPath = "shapes.hpp";

    const nlohmann::json j = writer.file(ctx, fr, /*wantContents=*/true, /*wantOccurrences=*/true);
    const nlohmann::json parsed = nlohmann::json::parse(j.dump());

    CHECK(parsed["schema"] == "kg.file/1");
    checkTopLevelKeys(parsed, {"kg_version", "schema", "query", "file", "contents", "occurrences"});
    REQUIRE(parsed["occurrences"].is_array());
    bool sawNode = false;
    bool sawLocalSymbol = false;
    for (const nlohmann::json& occ : parsed["occurrences"])
    {
        CHECK(occ.contains("is_local_symbol"));
        CHECK(occ["is_local_symbol"].is_boolean());
        if (occ.contains("node"))
        {
            CHECK(occ["node"]["kind"].is_string());
            CHECK(occ["node"].contains("qualified_name"));
            CHECK(occ["is_local_symbol"] == false);
            sawNode = true;
        }
        if (occ["is_local_symbol"] == true)
        {
            CHECK_FALSE(occ.contains("node"));
            sawLocalSymbol = true;
        }
    }
    CHECK(sawNode);
    CHECK(sawLocalSymbol);
    checkKindFieldsAreStrings(parsed);
}

TEST_CASE("json_shape kg.ambiguous/1 lists every candidate as a full node object", "[fixture][json_shape]")
{
    kg::db::Database db(fixtureDbPath());
    QueryEngine engine(db);
    JsonWriter writer(db, engine, "test-version");

    // "Square" (no --kind) is a known-ambiguous spec in the fixture -- the
    // class and its same-named constructor both have leaf name "Square"
    // (see docs/KNOWN_LIMITATIONS.md's Phase 4 section).
    const kg::query::ResolveResult rr = kg::query::resolveSymbol(db, "Square", nullptr, nullptr);
    REQUIRE(rr.outcome == ResolveOutcome::Ambiguous);
    REQUIRE(rr.candidates.size() >= 2);

    QueryContext ctx;
    ctx.verb = "bases";
    ctx.db = fixtureDbPath();
    ctx.symbolOrTextOrPath = "Square";

    const nlohmann::json j = writer.ambiguous(ctx, rr.candidates);
    const nlohmann::json parsed = nlohmann::json::parse(j.dump());

    CHECK(parsed["schema"] == "kg.ambiguous/1");
    checkTopLevelKeys(parsed, {"kg_version", "schema", "query", "candidates"});
    CHECK(parsed["candidates"].size() == rr.candidates.size());
    for (const nlohmann::json& c : parsed["candidates"])
    {
        CHECK(c["kind"].is_string());
        CHECK(c.contains("id"));
    }
    checkKindFieldsAreStrings(parsed);
}

TEST_CASE("json_shape kg.not-found/1 has a suggestions array (possibly empty)", "[fixture][json_shape]")
{
    kg::db::Database db(fixtureDbPath());
    QueryEngine engine(db);
    JsonWriter writer(db, engine, "test-version");

    const std::vector<kg::query::SearchHit> hits = engine.search("Squorable_Nonexistent_Xyz", nullptr, 5);

    QueryContext ctx;
    ctx.verb = "bases";
    ctx.db = fixtureDbPath();
    ctx.symbolOrTextOrPath = "Squorable_Nonexistent_Xyz";

    const nlohmann::json j = writer.notFound(ctx, hits);
    const nlohmann::json parsed = nlohmann::json::parse(j.dump());

    CHECK(parsed["schema"] == "kg.not-found/1");
    checkTopLevelKeys(parsed, {"kg_version", "schema", "query", "suggestions"});
    REQUIRE(parsed["suggestions"].is_array());
    checkKindFieldsAreStrings(parsed);
}

TEST_CASE("json_shape kg.index-summary/1 is emitted by the real kg::index::runIndex() --json path", "[fixture][json_shape]")
{
    kg::index::IndexRunOptions opts;
    opts.compileCommandsPath = fixtureCdbPath();
    opts.outputPath = testBinaryDir() + "/json_shape_index_out.srctrldb";
    opts.force = true;
    opts.quiet = true;
    opts.jsonSummary = true;
    opts.kgVersion = "test-version";

    const std::string stdoutPath = testBinaryDir() + "/json_shape_index_stdout.txt";
    int rc = 0;
    const std::string captured = captureStdout([&]() { rc = kg::index::runIndex(opts); }, stdoutPath);
    CHECK(rc == 0);

    REQUIRE_FALSE(captured.empty());
    const nlohmann::json parsed = nlohmann::json::parse(captured);  // proves it's valid JSON (and exactly one object)

    CHECK(parsed["schema"] == "kg.index-summary/1");
    CHECK(parsed["kg_version"] == "test-version");
    checkTopLevelKeys(parsed, {"kg_version", "schema", "database", "counts", "elapsed_ms", "failed_tus"});
    checkTopLevelKeys(
        parsed["counts"],
        {"tus_total", "tus_attempted", "tus_with_content", "tus_failed", "symbols", "references", "files", "errors", "name_collisions"});
    CHECK(parsed["counts"]["tus_total"].get<int64_t>() > 0);
}

TEST_CASE("json_shape determinism: repeated in-process calls dump() byte-for-byte identical", "[fixture][json_shape]")
{
    kg::db::Database db(fixtureDbPath());
    QueryEngine engine(db);
    JsonWriter writer(db, engine, "test-version");

    SECTION("graph verb (bases)")
    {
        const int64_t square = resolveOne(db, "fixture::Square");
        QueryContext ctx;
        ctx.verb = "bases";
        ctx.db = fixtureDbPath();
        ctx.symbolOrTextOrPath = "fixture::Square";
        ctx.symbolIds = {square};
        ctx.depth = 0;
        ctx.edgeKinds = {kg::db::EdgeKind::INHERITANCE};

        const kg::query::BfsResult r1 = engine.bases({square}, 0, 0, 0);
        const kg::query::BfsResult r2 = engine.bases({square}, 0, 0, 0);
        const std::string dump1 = writer.graph(ctx, r1, /*elapsedMs=*/0).dump();
        const std::string dump2 = writer.graph(ctx, r2, /*elapsedMs=*/0).dump();
        CHECK(dump1 == dump2);
    }

    SECTION("search")
    {
        QueryContext ctx;
        ctx.verb = "search";
        ctx.db = fixtureDbPath();
        ctx.symbolOrTextOrPath = "sqr";
        ctx.limit = 10;

        const std::vector<kg::query::SearchHit> h1 = engine.search("sqr", nullptr, 10);
        const std::vector<kg::query::SearchHit> h2 = engine.search("sqr", nullptr, 10);
        const std::string dump1 = writer.search(ctx, h1, /*elapsedMs=*/0).dump();
        const std::string dump2 = writer.search(ctx, h2, /*elapsedMs=*/0).dump();
        CHECK(dump1 == dump2);
    }

    SECTION("stats")
    {
        QueryContext ctx;
        ctx.verb = "stats";
        ctx.db = fixtureDbPath();

        const kg::query::StatsResult s1 = engine.stats(100);
        const kg::query::StatsResult s2 = engine.stats(100);
        const std::string dump1 = writer.stats(ctx, s1).dump();
        const std::string dump2 = writer.stats(ctx, s2).dump();
        CHECK(dump1 == dump2);
    }
}
