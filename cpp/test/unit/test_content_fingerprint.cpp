#include <catch2/catch_test_macros.hpp>

#include <filesystem>
#include <fstream>
#include <unistd.h>
#include <nlohmann/json.hpp>
#include <sqlite3.h>

#include "symhow/common/ContentHash.h"
#include "symhow/common/Process.h"
#include "symhow/db/Database.h"
#include "symhow/db/WritableDb.h"
#include "symhow/index/IndexDriver.h"
#include "symhow/index/SrctrldbBackend.h"
#include "symhow/update/Git.h"
#include "symhow/update/Snapshot.h"
#include "symhow/update/UpdateDriver.h"

namespace
{
struct Corpus
{
    std::filesystem::path root = std::filesystem::temp_directory_path() / ("symhow_fingerprint_" + std::to_string(::getpid()));
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
        const auto result = symhow::common::process::run(args);
        INFO(result.err);
        REQUIRE(result.exitCode == 0);
    }
    void commit()
    {
        git({"add", "-A"});
        git({"-c", "user.name=symhow", "-c", "user.email=symhow@test", "commit", "-qm", "snapshot"});
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
        symhow::index::IndexRunOptions opts;
        opts.compileCommandsPath = cdb;
        opts.outputPath = output.empty() ? db : output;
        opts.force = true;
        opts.quiet = opts.superQuiet = true;
        opts.parallel = parallel;
        opts.commitEvery = 1;
        symhow::index::Provenance prov;
        prov.cdbPath = cdb;
        prov.repoRoot = root.string();
        prov.repoCommit = *symhow::update::revParse(root.string(), "HEAD");
        prov.dirtyPaths = symhow::update::parseStatusPorcelainZ(*symhow::update::statusPorcelainZ(root.string()), root);
        opts.provenance = prov;
        REQUIRE(symhow::index::runIndex(opts) == 0);
    }
    symhow::update::UpdateRunOptions options()
    {
        symhow::update::UpdateRunOptions opts;
        opts.dbPath = db;
        return opts;
    }
    std::string scalar(const std::string& sql)
    {
        symhow::db::Database read(db);
        std::string result;
        read.run(sql, [&](sqlite3_stmt* stmt) {
            const char* value = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 0));
            result = value == nullptr ? "" : value;
        });
        return result;
    }
    void exec(const std::string& sql)
    {
        symhow::db::WritableDb writeDb(db, true);
        writeDb.exec(sql);
    }
    void equivalent()
    {
        const std::string fresh = (root / "fresh.srctrldb").string();
        index(fresh, 2);
        const auto diff = symhow::update::diffSnapshots(symhow::update::takeSnapshot(db), symhow::update::takeSnapshot(fresh));
        REQUIRE(diff.onlyInA.empty());
        REQUIRE(diff.onlyInB.empty());
    }
};
}  // namespace

TEST_CASE("fingerprint: dirty source/header updates become no-ops, but subsequent edits and reverts still index")
{
    Corpus corpus;
    corpus.index();
    REQUIRE(corpus.scalar("SELECT count(*) FROM symhow_file_fingerprint") == "2");
    auto opts = corpus.options();
    std::string file;
    std::string original;
    SECTION("source") { file = "main.cpp"; original = corpus.source; }
    SECTION("header") { file = "value.hpp"; original = corpus.header; }
    corpus.write(file, original + "int extra() { return 2; }\n");
    symhow::update::UpdateSummary first;
    REQUIRE(symhow::update::runUpdate(opts, &first) == 0);
    REQUIRE(first.tusToIndex == 1);
    REQUIRE(symhow::update::runUpdate(opts) == 5);
    corpus.equivalent();

    const auto timestamp = std::filesystem::last_write_time(corpus.root / file);
    corpus.write(file, original + "int extra() { return 3; }\n"); // same size, same mtime
    std::filesystem::last_write_time(corpus.root / file, timestamp);
    REQUIRE(symhow::update::runUpdate(opts) == 0);
    REQUIRE(symhow::update::runUpdate(opts) == 5);
    corpus.equivalent();

    corpus.write(file, original); // git is now clean, but indexed content was dirty
    REQUIRE(symhow::update::runUpdate(opts) == 0);
    REQUIRE(symhow::update::runUpdate(opts) == 5);
    corpus.equivalent();
    REQUIRE(corpus.scalar("SELECT count(*) FROM symhow_file_fingerprint fp LEFT JOIN file f ON f.id=fp.file_node_id WHERE f.id IS NULL") == "0");
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
        corpus.git({"-c", "user.name=symhow", "-c", "user.email=symhow@test", "commit", "-qm", "untrack TU"});
    }
    corpus.index();
    REQUIRE(corpus.scalar("SELECT sha256 FROM symhow_file_fingerprint fp JOIN file f ON f.id=fp.file_node_id WHERE f.path LIKE '%value.hpp'") ==
        *symhow::common::fileContentHash((corpus.root / "value.hpp").string()));
    auto opts = corpus.options();
    const auto before = symhow::common::fileContentHash(corpus.db);
    opts.dryRun = true;
    REQUIRE(symhow::update::runUpdate(opts) == 5);
    REQUIRE(symhow::common::fileContentHash(corpus.db) == before);
    opts.dryRun = false;
    REQUIRE(symhow::update::runUpdate(opts) == 5);
    corpus.commit(); // committing already indexed bytes must not trigger parsing
    REQUIRE(symhow::update::runUpdate(opts) == 5);
    REQUIRE(corpus.scalar("SELECT value FROM meta WHERE key='symhow_repo_commit'") == *symhow::update::revParse(corpus.root.string(), "HEAD"));
}

TEST_CASE("fingerprint: unchanged bytes never suppress forced files, flags, retries or explicit revisions")
{
    Corpus corpus;
    const std::string baseline = *symhow::update::revParse(corpus.root.string(), "HEAD");
    corpus.write("main.cpp", corpus.source + "int extra;\n");
    corpus.index();
    auto opts = corpus.options();
    REQUIRE(symhow::update::runUpdate(opts) == 5);
    symhow::update::UpdateSummary summary;
    SECTION("forced file") { opts.explicitFiles = {(corpus.root / "main.cpp").string()}; }
    SECTION("compiler flags") { corpus.compile("-DVALUE=2"); }
    SECTION("partial retry") { corpus.exec("UPDATE symhow_tu SET status='partial'"); }
    SECTION("failed retry") { corpus.exec("UPDATE symhow_tu SET status='failed'"); }
    SECTION("explicit from revision") { corpus.commit(); opts.fromRev = baseline; }
    SECTION("explicit to revision") { corpus.commit(); opts.toRev = "HEAD"; }
    REQUIRE(symhow::update::runUpdate(opts, &summary) == 0);
    REQUIRE(summary.tusToIndex == 1);
    corpus.equivalent();
}

TEST_CASE("fingerprint: legacy, missing and mismatched fingerprints conservatively re-index once")
{
    Corpus corpus;
    corpus.write("main.cpp", corpus.source + "int extra;\n");
    corpus.index();
    SECTION("legacy database") { corpus.exec("DROP TABLE symhow_file_fingerprint"); }
    SECTION("missing row") { corpus.exec("DELETE FROM symhow_file_fingerprint"); }
    SECTION("mismatched hash") { corpus.exec("UPDATE symhow_file_fingerprint SET sha256='unknown'"); }
    auto opts = corpus.options();
    REQUIRE(symhow::update::runUpdate(opts) == 0);
    REQUIRE(symhow::update::runUpdate(opts) == 5);
    corpus.equivalent();
}

TEST_CASE("fingerprint: a known header newly added to the compilation database must still be indexed as a TU")
{
    Corpus corpus;
    corpus.write("value.hpp", corpus.header + "int extra;\n");
    corpus.index();
    corpus.compile("-DVALUE=1", true);
    auto opts = corpus.options();
    symhow::update::UpdateSummary summary;
    REQUIRE(symhow::update::runUpdate(opts, &summary) == 0);
    REQUIRE(summary.tusToIndex >= 1);
    REQUIRE(corpus.scalar("SELECT count(*) FROM symhow_tu") == "2");
    REQUIRE(symhow::update::runUpdate(opts) == 5);
    corpus.equivalent();
}

TEST_CASE("fingerprint: conflicting parsed snapshots across checkpoints disable skipping")
{
    Corpus corpus;
    symhow::index::SrctrldbBackend backend;
    std::string err;
    REQUIRE(backend.begin(corpus.db, &err));
    REQUIRE(backend.startTransaction(&err));
    auto consume = [&](const std::string& bytes) {
        symhow::model::TuFactBatch batch;
        symhow::model::FileFact file;
        file.path = (corpus.root / "main.cpp").string();
        file.indexed = true;
        file.contentHash = symhow::common::contentHash(bytes);
        batch.files.push_back(file);
        REQUIRE(backend.consume(batch, &err));
        REQUIRE(backend.checkpoint({}, &err));
    };
    consume("first parsed snapshot");
    REQUIRE(corpus.scalar("SELECT count(*) FROM symhow_file_fingerprint") == "1");
    REQUIRE(corpus.scalar("SELECT sha256 FROM symhow_file_fingerprint") == symhow::common::contentHash("first parsed snapshot"));
    consume("second parsed snapshot");
    REQUIRE(corpus.scalar("SELECT count(*) FROM symhow_file_fingerprint") == "0");
    consume("first parsed snapshot"); // a third TU must not rehabilitate mixed facts
    REQUIRE(corpus.scalar("SELECT count(*) FROM symhow_file_fingerprint") == "0");
    symhow::model::IndexSummary summary;
    REQUIRE(backend.finish(summary, &err));
}

TEST_CASE("fingerprint: surviving file facts cannot be certified with new or previously unknown bytes")
{
    Corpus corpus;
    auto append = [&](const std::string& bytes) {
        symhow::index::SrctrldbBackend backend;
        std::string err;
        REQUIRE(backend.begin(corpus.db, &err));
        REQUIRE(backend.startTransaction(&err));
        symhow::model::TuFactBatch batch;
        symhow::model::FileFact file;
        file.path = (corpus.root / "main.cpp").string();
        file.indexed = true;
        file.contentHash = symhow::common::contentHash(bytes);
        batch.files.push_back(file);
        REQUIRE(backend.consume(batch, &err));
        REQUIRE(backend.checkpoint({}, &err));
        symhow::model::IndexSummary summary;
        REQUIRE(backend.finish(summary, &err));
    };
    append("old parsed bytes");
    append("old parsed bytes");
    REQUIRE(corpus.scalar("SELECT count(*) FROM symhow_file_fingerprint") == "1");
    append("new parsed bytes"); // old facts survived; this is NOT full invalidation
    REQUIRE(corpus.scalar("SELECT count(*) FROM symhow_file_fingerprint") == "0");
    append("new parsed bytes"); // missing hash must not certify the old/mixed facts
    REQUIRE(corpus.scalar("SELECT count(*) FROM symhow_file_fingerprint") == "0");
}
