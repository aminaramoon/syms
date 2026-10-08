#include <catch2/catch_test_macros.hpp>

#include <filesystem>
#include <fstream>
#include <unistd.h>
#include <nlohmann/json.hpp>
#include <sqlite3.h>

#include "kg/common/ContentHash.h"
#include "kg/common/Process.h"
#include "kg/db/Database.h"
#include "kg/db/WritableDb.h"
#include "kg/index/IndexDriver.h"
#include "kg/index/SrctrldbBackend.h"
#include "kg/update/Git.h"
#include "kg/update/Snapshot.h"
#include "kg/update/UpdateDriver.h"

namespace
{
struct Corpus
{
    std::filesystem::path root = std::filesystem::temp_directory_path() / ("kg_fingerprint_" + std::to_string(::getpid()));
    std::string db = (root / "index.srctrldb").string();
    std::string cdb = (root / "compile_commands.json").string();
    const std::string source = "#include \"value.hpp\"\nint run() { return value(); }\n";
    const std::string header = "inline int value() { return 1; }\n";

    Corpus()
    {
        std::filesystem::remove_all(root);
        std::filesystem::create_directories(root);
        write("main.cpp", source);
        write("value.hpp", header);
        write(".gitignore", "*.srctrldb*\ncompile_commands.json\n");
        git({"init", "-q"});
        commit();
        compile();
    }
    ~Corpus() { std::filesystem::remove_all(root); }

    void write(const std::string& name, const std::string& bytes)
    {
        std::ofstream out(root / name, std::ios::binary);
        out.write(bytes.data(), bytes.size());
        REQUIRE(out.good());
    }
    void git(std::vector<std::string> args)
    {
        args.insert(args.begin(), {"git", "-C", root.string()});
        const auto result = kg::common::process::run(args);
        INFO(result.err);
        REQUIRE(result.exitCode == 0);
    }
    void commit()
    {
        git({"add", "-A"});
        git({"-c", "user.name=kg", "-c", "user.email=kg@test", "commit", "-qm", "snapshot"});
    }
    void compile(const std::string& flag = "-DVALUE=1", bool headerTu = false)
    {
        nlohmann::json entries = nlohmann::json::array();
        for (const std::string& name : headerTu ? std::vector<std::string>{"main.cpp", "value.hpp"} : std::vector<std::string>{"main.cpp"})
        {
            const std::string file = (root / name).string();
            entries.push_back({{"directory", root.string()}, {"file", file},
                {"arguments", {"clang++", "-x", "c++", "-std=c++20", flag, "-I" + root.string(), file}}});
        }
        write("compile_commands.json", entries.dump());
    }
    void index(const std::string& output = "", int parallel = 1)
    {
        kg::index::IndexRunOptions opts;
        opts.compileCommandsPath = cdb;
        opts.outputPath = output.empty() ? db : output;
        opts.force = true;
        opts.quiet = opts.superQuiet = true;
        opts.parallel = parallel;
        opts.commitEvery = 1;
        kg::index::Provenance prov;
        prov.cdbPath = cdb;
        prov.repoRoot = root.string();
        prov.repoCommit = *kg::update::revParse(root.string(), "HEAD");
        prov.dirtyPaths = kg::update::parseStatusPorcelainZ(*kg::update::statusPorcelainZ(root.string()), root);
        opts.provenance = prov;
        REQUIRE(kg::index::runIndex(opts) == 0);
    }
    kg::update::UpdateRunOptions options()
    {
        kg::update::UpdateRunOptions opts;
        opts.dbPath = db;
        return opts;
    }
    std::string scalar(const std::string& sql)
    {
        kg::db::Database read(db);
        std::string result;
        read.run(sql, [&](sqlite3_stmt* stmt) {
            const char* value = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 0));
            result = value == nullptr ? "" : value;
        });
        return result;
    }
    void exec(const std::string& sql)
    {
        kg::db::WritableDb writeDb(db, true);
        writeDb.exec(sql);
    }
    void equivalent()
    {
        const std::string fresh = (root / "fresh.srctrldb").string();
        index(fresh, 2);
        const auto diff = kg::update::diffSnapshots(kg::update::takeSnapshot(db), kg::update::takeSnapshot(fresh));
        REQUIRE(diff.onlyInA.empty());
        REQUIRE(diff.onlyInB.empty());
    }
};
}  // namespace

TEST_CASE("fingerprint: dirty source/header updates become no-ops, but subsequent edits and reverts still index")
{
    Corpus corpus;
    corpus.index();
    REQUIRE(corpus.scalar("SELECT count(*) FROM kg_file_fingerprint") == "2");
    auto opts = corpus.options();
    std::string file;
    std::string original;
    SECTION("source") { file = "main.cpp"; original = corpus.source; }
    SECTION("header") { file = "value.hpp"; original = corpus.header; }
    corpus.write(file, original + "int extra() { return 2; }\n");
    kg::update::UpdateSummary first;
    REQUIRE(kg::update::runUpdate(opts, &first) == 0);
    REQUIRE(first.tusToIndex == 1);
    REQUIRE(kg::update::runUpdate(opts) == 5);
    corpus.equivalent();

    const auto timestamp = std::filesystem::last_write_time(corpus.root / file);
    corpus.write(file, original + "int extra() { return 3; }\n"); // same size, same mtime
    std::filesystem::last_write_time(corpus.root / file, timestamp);
    REQUIRE(kg::update::runUpdate(opts) == 0);
    REQUIRE(kg::update::runUpdate(opts) == 5);
    corpus.equivalent();

    corpus.write(file, original); // git is now clean, but indexed content was dirty
    REQUIRE(kg::update::runUpdate(opts) == 0);
    REQUIRE(kg::update::runUpdate(opts) == 5);
    corpus.equivalent();
    REQUIRE(corpus.scalar("SELECT count(*) FROM kg_file_fingerprint fp LEFT JOIN file f ON f.id=fp.file_node_id WHERE f.id IS NULL") == "0");
}

TEST_CASE("fingerprint: full index of dirty/untracked content already has usable parsed-byte fingerprints")
{
    Corpus corpus;
    SECTION("dirty source") { corpus.write("main.cpp", corpus.source + "int extra;\n"); }
    SECTION("dirty CRLF header") { corpus.write("value.hpp", "inline int value() { return 2; }\r\n"); }
    SECTION("dirty header without final newline") { corpus.write("value.hpp", "inline int value() { return 2; }"); }
    SECTION("untracked TU")
    {
        corpus.git({"rm", "--cached", "main.cpp"});
        corpus.git({"-c", "user.name=kg", "-c", "user.email=kg@test", "commit", "-qm", "untrack TU"});
    }
    corpus.index();
    REQUIRE(corpus.scalar("SELECT sha256 FROM kg_file_fingerprint fp JOIN file f ON f.id=fp.file_node_id WHERE f.path LIKE '%value.hpp'") ==
        *kg::common::fileContentHash((corpus.root / "value.hpp").string()));
    auto opts = corpus.options();
    const auto before = kg::common::fileContentHash(corpus.db);
    opts.dryRun = true;
    REQUIRE(kg::update::runUpdate(opts) == 5);
    REQUIRE(kg::common::fileContentHash(corpus.db) == before);
    opts.dryRun = false;
    REQUIRE(kg::update::runUpdate(opts) == 5);
    corpus.commit(); // committing already indexed bytes must not trigger parsing
    REQUIRE(kg::update::runUpdate(opts) == 5);
    REQUIRE(corpus.scalar("SELECT value FROM meta WHERE key='kg_repo_commit'") == *kg::update::revParse(corpus.root.string(), "HEAD"));
}

TEST_CASE("fingerprint: unchanged bytes never suppress forced files, flags, retries or explicit revisions")
{
    Corpus corpus;
    const std::string baseline = *kg::update::revParse(corpus.root.string(), "HEAD");
    corpus.write("main.cpp", corpus.source + "int extra;\n");
    corpus.index();
    auto opts = corpus.options();
    REQUIRE(kg::update::runUpdate(opts) == 5);
    kg::update::UpdateSummary summary;
    SECTION("forced file") { opts.explicitFiles = {(corpus.root / "main.cpp").string()}; }
    SECTION("compiler flags") { corpus.compile("-DVALUE=2"); }
    SECTION("partial retry") { corpus.exec("UPDATE kg_tu SET status='partial'"); }
    SECTION("failed retry") { corpus.exec("UPDATE kg_tu SET status='failed'"); }
    SECTION("explicit from revision") { corpus.commit(); opts.fromRev = baseline; }
    SECTION("explicit to revision") { corpus.commit(); opts.toRev = "HEAD"; }
    REQUIRE(kg::update::runUpdate(opts, &summary) == 0);
    REQUIRE(summary.tusToIndex == 1);
    corpus.equivalent();
}

TEST_CASE("fingerprint: legacy, missing and mismatched fingerprints conservatively re-index once")
{
    Corpus corpus;
    corpus.write("main.cpp", corpus.source + "int extra;\n");
    corpus.index();
    SECTION("legacy database") { corpus.exec("DROP TABLE kg_file_fingerprint"); }
    SECTION("missing row") { corpus.exec("DELETE FROM kg_file_fingerprint"); }
    SECTION("mismatched hash") { corpus.exec("UPDATE kg_file_fingerprint SET sha256='unknown'"); }
    auto opts = corpus.options();
    REQUIRE(kg::update::runUpdate(opts) == 0);
    REQUIRE(kg::update::runUpdate(opts) == 5);
    corpus.equivalent();
}

TEST_CASE("fingerprint: a known header newly added to the compilation database must still be indexed as a TU")
{
    Corpus corpus;
    corpus.write("value.hpp", corpus.header + "int extra;\n");
    corpus.index();
    corpus.compile("-DVALUE=1", true);
    auto opts = corpus.options();
    kg::update::UpdateSummary summary;
    REQUIRE(kg::update::runUpdate(opts, &summary) == 0);
    REQUIRE(summary.tusToIndex >= 1);
    REQUIRE(corpus.scalar("SELECT count(*) FROM kg_tu") == "2");
    REQUIRE(kg::update::runUpdate(opts) == 5);
    corpus.equivalent();
}

TEST_CASE("fingerprint: conflicting parsed snapshots across checkpoints disable skipping")
{
    Corpus corpus;
    kg::index::SrctrldbBackend backend;
    std::string err;
    REQUIRE(backend.begin(corpus.db, &err));
    REQUIRE(backend.startTransaction(&err));
    auto consume = [&](const std::string& bytes) {
        kg::model::TuFactBatch batch;
        kg::model::FileFact file;
        file.path = (corpus.root / "main.cpp").string();
        file.indexed = true;
        file.contentHash = kg::common::contentHash(bytes);
        batch.files.push_back(file);
        REQUIRE(backend.consume(batch, &err));
        REQUIRE(backend.checkpoint({}, &err));
    };
    consume("first parsed snapshot");
    REQUIRE(corpus.scalar("SELECT count(*) FROM kg_file_fingerprint") == "1");
    REQUIRE(corpus.scalar("SELECT sha256 FROM kg_file_fingerprint") == kg::common::contentHash("first parsed snapshot"));
    consume("second parsed snapshot");
    REQUIRE(corpus.scalar("SELECT count(*) FROM kg_file_fingerprint") == "0");
    consume("first parsed snapshot"); // a third TU must not rehabilitate mixed facts
    REQUIRE(corpus.scalar("SELECT count(*) FROM kg_file_fingerprint") == "0");
    kg::model::IndexSummary summary;
    REQUIRE(backend.finish(summary, &err));
}

TEST_CASE("fingerprint: surviving file facts cannot be certified with new or previously unknown bytes")
{
    Corpus corpus;
    auto append = [&](const std::string& bytes) {
        kg::index::SrctrldbBackend backend;
        std::string err;
        REQUIRE(backend.begin(corpus.db, &err));
        REQUIRE(backend.startTransaction(&err));
        kg::model::TuFactBatch batch;
        kg::model::FileFact file;
        file.path = (corpus.root / "main.cpp").string();
        file.indexed = true;
        file.contentHash = kg::common::contentHash(bytes);
        batch.files.push_back(file);
        REQUIRE(backend.consume(batch, &err));
        REQUIRE(backend.checkpoint({}, &err));
        kg::model::IndexSummary summary;
        REQUIRE(backend.finish(summary, &err));
    };
    append("old parsed bytes");
    append("old parsed bytes");
    REQUIRE(corpus.scalar("SELECT count(*) FROM kg_file_fingerprint") == "1");
    append("new parsed bytes"); // old facts survived; this is NOT full invalidation
    REQUIRE(corpus.scalar("SELECT count(*) FROM kg_file_fingerprint") == "0");
    append("new parsed bytes"); // missing hash must not certify the old/mixed facts
    REQUIRE(corpus.scalar("SELECT count(*) FROM kg_file_fingerprint") == "0");
}
