// Phase 6 real-corpus integration test: exercises the ENTIRE `kg` pipeline
// (real `kg index` process, via the compiled binary, against the actual
// cucs corpus, followed by real `kg query`-engine assertions against the
// resulting database) end to end. See docs/KNOWN_LIMITATIONS.md's Phase 6
// section and README.md for how this is wired into CTest.
//
// This is a plain `main()` (not a Catch2 test runner): the very first thing
// it must do is `exit(77)` if the cucs corpus isn't checked out at
// KG_CUCS_DIR -- CMake's SKIP_RETURN_CODE 77 convention lets CTest report
// this test as SKIPPED (not FAILED) on any machine that doesn't have the
// corpus. A Catch2 session's own startup (argument parsing, reporter setup,
// etc.) is real work that would have to happen before any TEST_CASE body
// could run that check, so a hand-rolled main() with a flat sequence of
// numbered assertions -- printing a clear [PASS]/[FAIL] line per assertion,
// exactly mirroring the 10 assertions in the Phase 6 design -- is simpler
// and keeps the "exit 77 before doing any real work" contract obviously
// correct by inspection.

#include <cstdio>
#include <cstdlib>
#include <chrono>
#include <filesystem>
#include <fstream>
#include <optional>
#include <set>
#include <sstream>
#include <string>
#include <vector>

#include <sys/wait.h>

#include <nlohmann/json.hpp>
#include <sqlite3.h>

#include "kg/db/Database.h"
#include "kg/db/Enums.h"
#include "kg/db/NameDecode.h"
#include "kg/query/QueryEngine.h"
#include "kg/query/SymbolResolver.h"

#ifndef KG_CUCS_DIR
#define KG_CUCS_DIR ""
#endif
#ifndef KG_BINARY
#define KG_BINARY ""
#endif
#ifndef KG_TEST_OUTPUT_DIR
#define KG_TEST_OUTPUT_DIR "."
#endif

namespace fs = std::filesystem;

namespace
{

int g_pass = 0;
int g_fail = 0;

void check(bool condition, const std::string& description)
{
    if (condition)
    {
        std::printf("[PASS] %s\n", description.c_str());
        ++g_pass;
    }
    else
    {
        std::printf("[FAIL] %s\n", description.c_str());
        ++g_fail;
    }
}

std::string readFile(const std::string& path)
{
    std::ifstream in(path, std::ios::binary);
    std::ostringstream oss;
    oss << in.rdbuf();
    return oss.str();
}

// Last `n` lines of `text` (for printing a bounded stderr tail into CTest's
// own captured output, rather than dumping potentially thousands of
// per-TU progress lines).
std::string lastLines(const std::string& text, std::size_t n)
{
    std::vector<std::string> lines;
    std::istringstream iss(text);
    std::string line;
    while (std::getline(iss, line))
    {
        lines.push_back(line);
    }
    const std::size_t start = lines.size() > n ? lines.size() - n : 0;
    std::ostringstream oss;
    for (std::size_t i = start; i < lines.size(); ++i)
    {
        oss << "    | " << lines[i] << "\n";
    }
    return oss.str();
}

// Resolves `spec` to a single node id via kg::query::resolveSymbol(),
// printing a [FAIL] line (and returning std::nullopt) if it isn't Unique.
std::optional<int64_t> resolveUnique(
    kg::db::Database& db, const std::string& spec, const std::vector<int>* kindFilter, const std::string& label)
{
    const kg::query::ResolveResult rr = kg::query::resolveSymbol(db, spec, kindFilter, nullptr);
    if (rr.outcome != kg::query::ResolveOutcome::Unique)
    {
        std::printf(
            "[FAIL] %s: resolveSymbol('%s') expected Unique, got %s (%zu candidates)\n",
            label.c_str(),
            spec.c_str(),
            rr.outcome == kg::query::ResolveOutcome::Ambiguous ? "Ambiguous" : "NotFound",
            rr.candidates.size());
        ++g_fail;
        return std::nullopt;
    }
    return rr.candidates.front().id;
}

// The non-seed (depth > 0) qualified names (kg::db::DecodedName::namePath())
// of a BfsResult -- "X's bases/derived" as a SET, per docs/KNOWN_LIMITATIONS.md's
// Phase 4 note that BfsResult::nodes always includes the seed(s) at depth 0.
std::set<std::string> nonSeedQualifiedNames(const kg::query::BfsResult& r)
{
    std::set<std::string> names;
    for (const auto& n : r.nodes)
    {
        if (n.depth > 0)
        {
            names.insert(kg::db::decodeSerializedName(n.serializedName).namePath());
        }
    }
    return names;
}

bool containsNode(const kg::query::BfsResult& r, int64_t id)
{
    for (const auto& n : r.nodes)
    {
        if (n.id == id)
        {
            return true;
        }
    }
    return false;
}

// Runs `command` via the shell, redirecting stdout/stderr to separate files
// (so both are captured for assertions AND remain readable/greppable
// afterwards for human-visible CTest output-on-failure). Returns the
// process's real exit code (or -1 if it didn't exit normally, e.g. killed
// by a signal).
int runCaptured(const std::string& command, const std::string& stdoutPath, const std::string& stderrPath)
{
    const std::string full = command + " > \"" + stdoutPath + "\" 2> \"" + stderrPath + "\"";
    const int raw = std::system(full.c_str());
    if (raw == -1)
    {
        return -1;
    }
    if (WIFEXITED(raw))
    {
        return WEXITSTATUS(raw);
    }
    return -1;  // killed/signaled -- definitely not a clean exit
}

}  // namespace

int main()
{
    const std::string cucsDir = KG_CUCS_DIR;
    const std::string cdbPath = cucsDir + "/compile_commands.json";

    // --- exit(77) contract: must happen before any real work. -------------
    if (cucsDir.empty() || !fs::exists(cdbPath))
    {
        std::fprintf(
            stderr,
            "cucs_integration: '%s' not found -- skipping (this test only runs on a machine with the cucs corpus "
            "checked out at KG_CUCS_DIR).\n",
            cdbPath.c_str());
        return 77;
    }

    std::printf("cucs_integration: KG_CUCS_DIR=%s\n", cucsDir.c_str());
    std::printf("cucs_integration: kg binary=%s\n", std::string(KG_BINARY).c_str());

    // Ground truth for assertion 1: count `"file":` entries in the CDB
    // itself, rather than trusting a possibly-stale hardcoded number (see
    // the Phase 6 design note: the corpus had 200 TUs when the design was
    // written, but the test re-derives this from the CDB on every run so
    // it can never silently go stale).
    std::size_t realTuCount = 0;
    try
    {
        const nlohmann::json cdb = nlohmann::json::parse(readFile(cdbPath));
        realTuCount = cdb.size();
    }
    catch (const std::exception& e)
    {
        std::printf("[FAIL] parsing %s as JSON: %s\n", cdbPath.c_str(), e.what());
        std::printf("cucs_integration: SUMMARY %d passed, %d failed\n", g_pass, ++g_fail);
        return 1;
    }
    std::printf("cucs_integration: real compile_commands.json TU count = %zu\n", realTuCount);

    // ========================================================================
    // Step 1: `kg index` against the real corpus, shelling out to the real
    // compiled `kg` binary (so this exercises the exact same code path a
    // user invoking the CLI would), capturing stdout (the --json summary)
    // and stderr (per-TU progress, including the CUDA rung line) separately.
    // ========================================================================
    const std::string outDb = std::string(KG_TEST_OUTPUT_DIR) + "/cucs.srctrldb";
    const std::string stdoutPath = std::string(KG_TEST_OUTPUT_DIR) + "/cucs_index_stdout.json";
    const std::string stderrPath = std::string(KG_TEST_OUTPUT_DIR) + "/cucs_index_stderr.log";

    std::ostringstream cmd;
    cmd << "\"" << std::string(KG_BINARY) << "\" index -c \"" << cdbPath << "\" -o \"" << outDb
        << "\" --force --remap /mnt/workspace=/home/aaramoon/workspace -v --json";

    std::printf("cucs_integration: running: %s\n", cmd.str().c_str());
    const auto t0 = std::chrono::steady_clock::now();
    const int indexRc = runCaptured(cmd.str(), stdoutPath, stderrPath);
    const auto t1 = std::chrono::steady_clock::now();
    const double indexWallSeconds = std::chrono::duration<double>(t1 - t0).count();

    const std::string stdoutText = readFile(stdoutPath);
    const std::string stderrText = readFile(stderrPath);

    std::printf("cucs_integration: kg index exit code = %d, wall time = %.1fs\n", indexRc, indexWallSeconds);
    std::printf("cucs_integration: kg index stderr (last 15 lines):\n%s", lastLines(stderrText, 15).c_str());

    check(indexRc == 0, "[3] kg index process exits 0");
    check(indexWallSeconds < 3600.0, "[3] kg index completes within the 3600s CTest timeout (self-measured)");
    std::printf("cucs_integration: [3] actual wall time = %.1fs\n", indexWallSeconds);

    nlohmann::json summary;
    bool haveSummary = false;
    try
    {
        summary = nlohmann::json::parse(stdoutText);
        haveSummary = true;
    }
    catch (const std::exception& e)
    {
        std::printf("[FAIL] parsing kg index --json stdout as JSON: %s\nstdout was:\n%s\n", e.what(), stdoutText.c_str());
        ++g_fail;
    }

    if (haveSummary)
    {
        const auto& counts = summary["counts"];
        const int64_t tusTotal = counts.value("tus_total", int64_t{-1});
        const int64_t tusAttempted = counts.value("tus_attempted", int64_t{-1});
        const int64_t tusWithContent = counts.value("tus_with_content", int64_t{-1});

        std::printf(
            "cucs_integration: summary.counts = {tus_total=%lld, tus_attempted=%lld, tus_with_content=%lld}\n",
            static_cast<long long>(tusTotal),
            static_cast<long long>(tusAttempted),
            static_cast<long long>(tusWithContent));

        check(
            tusTotal == static_cast<int64_t>(realTuCount),
            "[1] summary.counts.tus_total == real compile_commands.json entry count");
        check(
            tusAttempted == static_cast<int64_t>(realTuCount),
            "[1] summary.counts.tus_attempted == real compile_commands.json entry count");
        check(tusWithContent >= 190, "[2] summary.counts.tus_with_content >= 190");
    }
    else
    {
        check(false, "[1] summary.counts.tus_total == real compile_commands.json entry count");
        check(false, "[1] summary.counts.tus_attempted == real compile_commands.json entry count");
        check(false, "[2] summary.counts.tus_with_content >= 190");
    }

    if (!fs::exists(outDb))
    {
        std::printf("cucs_integration: FATAL: %s was not produced; cannot run query-side assertions [4]-[10].\n", outDb.c_str());
        std::printf("cucs_integration: SUMMARY %d passed, %d failed\n", g_pass, g_fail + 7);
        return 1;
    }

    // ========================================================================
    // Step 2: `kg query`-equivalent assertions, via kg::db::Database +
    // kg::query::QueryEngine/resolveSymbol linked directly into this test
    // binary (faster and more precise than re-parsing `kg query ... --json`
    // text output for every one of assertions [4]-[9]; Step 1 above already
    // exercised the CLI/process-shelling path for the `kg index` side).
    // ========================================================================
    kg::db::Database db(outDb);
    kg::query::QueryEngine engine(db);

    // --- [4] cucascade / cucascade::memory exist as NAMESPACE nodes -------
    {
        const std::vector<int> nsKind = {kg::db::NodeKind::NAMESPACE};
        const auto cucascade = resolveUnique(db, "cucascade", &nsKind, "[4]");
        const auto cucascadeMemory = resolveUnique(db, "cucascade::memory", &nsKind, "[4]");
        check(cucascade.has_value(), "[4] 'cucascade' resolves uniquely to a NAMESPACE node");
        check(cucascadeMemory.has_value(), "[4] 'cucascade::memory' resolves uniquely to a NAMESPACE node");
    }

    // --- [5] derived(reservation_limit_policy, depth=1) superset + location checks ---
    {
        const auto base = resolveUnique(db, "cucascade::memory::reservation_limit_policy", nullptr, "[5]");
        if (base)
        {
            const kg::query::BfsResult r = engine.derived({*base}, 1, 0);
            const std::set<std::string> names = nonSeedQualifiedNames(r);
            const std::set<std::string> expected = {
                "cucascade::memory::ignore_reservation_limit_policy",
                "cucascade::memory::fail_reservation_limit_policy",
                "cucascade::memory::increase_reservation_limit_policy",
            };
            bool superset = true;
            for (const auto& e : expected)
            {
                if (names.find(e) == names.end())
                {
                    superset = false;
                }
            }
            check(
                superset,
                "[5] derived(reservation_limit_policy, depth=1) is a superset of {ignore_,fail_,increase_}reservation_limit_policy");

            const kg::query::DefinitionResult baseDef = engine.definition({*base});
            bool baseLocOk = false;
            for (const auto& loc : baseDef.locations)
            {
                if (loc.filePath.find("include/cucascade/memory/memory_reservation.hpp") != std::string::npos && loc.startLine == 76)
                {
                    baseLocOk = true;
                }
            }
            check(baseLocOk, "[5] reservation_limit_policy's own definition is at memory_reservation.hpp:76");

            struct Expect
            {
                std::string spec;
                int line;
            };
            const std::vector<Expect> derivedExpect = {
                {"cucascade::memory::ignore_reservation_limit_policy", 113},
                {"cucascade::memory::fail_reservation_limit_policy", 131},
                {"cucascade::memory::increase_reservation_limit_policy", 149},
            };
            for (const auto& d : derivedExpect)
            {
                const auto id = resolveUnique(db, d.spec, nullptr, "[5]");
                bool ok = false;
                if (id)
                {
                    const kg::query::DefinitionResult def = engine.definition({*id});
                    for (const auto& loc : def.locations)
                    {
                        if (loc.filePath.find("include/cucascade/memory/memory_reservation.hpp") != std::string::npos &&
                            loc.startLine == d.line)
                        {
                            ok = true;
                        }
                    }
                }
                check(ok, "[5] " + d.spec + "'s definition is at memory_reservation.hpp:" + std::to_string(d.line));
            }
        }
        else
        {
            check(false, "[5] derived(reservation_limit_policy, depth=1) superset check");
        }
    }

    // --- [6] bases(increase_reservation_limit_policy, depth=1) == exactly {reservation_limit_policy} ---
    {
        const auto id = resolveUnique(db, "cucascade::memory::increase_reservation_limit_policy", nullptr, "[6]");
        if (id)
        {
            const kg::query::BfsResult r = engine.bases({*id}, 1, 0);
            const std::set<std::string> names = nonSeedQualifiedNames(r);
            const std::set<std::string> expected = {"cucascade::memory::reservation_limit_policy"};
            check(
                names == expected,
                "[6] bases(increase_reservation_limit_policy, depth=1) == exactly {reservation_limit_policy}");
        }
        else
        {
            check(false, "[6] bases(increase_reservation_limit_policy, depth=1) == exactly {reservation_limit_policy}");
        }
    }

    // --- [7] >= 1 OVERRIDE edge targeting reservation_limit_policy::get_policy_name ---
    {
        const auto id = resolveUnique(db, "cucascade::memory::reservation_limit_policy::get_policy_name", nullptr, "[7]");
        if (id)
        {
            const kg::query::BfsResult r = engine.overriddenBy({*id});
            int overrideCount = 0;
            for (const auto& e : r.edges)
            {
                if (e.kind == kg::db::EdgeKind::OVERRIDE && e.target == *id)
                {
                    ++overrideCount;
                }
            }
            std::printf("cucs_integration: [7] found %d OVERRIDE edge(s) targeting get_policy_name\n", overrideCount);
            check(overrideCount >= 1, "[7] at least 1 OVERRIDE edge targets reservation_limit_policy::get_policy_name");
        }
        else
        {
            check(false, "[7] at least 1 OVERRIDE edge targets reservation_limit_policy::get_policy_name");
        }
    }

    // --- [8] bases(disk_data_representation, depth=0) contains idata_representation ---
    {
        const auto id = resolveUnique(db, "cucascade::disk_data_representation", nullptr, "[8]");
        if (id)
        {
            const kg::query::BfsResult r = engine.bases({*id}, 0, 0);
            const std::set<std::string> names = nonSeedQualifiedNames(r);
            check(
                names.find("cucascade::idata_representation") != names.end(),
                "[8] bases(disk_data_representation, depth=0) contains cucascade::idata_representation");
        }
        else
        {
            check(false, "[8] bases(disk_data_representation, depth=0) contains cucascade::idata_representation");
        }
    }

    // --- [9] monotonic callers(f, depth=1) <= callers(f, depth=2) <= callers(f, depth=3) ---
    {
        // cucascade::data_batch::make: a real, frequently-called first-party
        // factory method (verified to have >100 distinct callers across the
        // corpus at the time this test was written -- see the Phase 6
        // report for how it was picked).
        const auto id = resolveUnique(db, "cucascade::data_batch::make", nullptr, "[9]");
        if (id)
        {
            const auto c1 = static_cast<int64_t>(engine.callers({*id}, 1, 0).nodes.size());
            const auto c2 = static_cast<int64_t>(engine.callers({*id}, 2, 0).nodes.size());
            const auto c3 = static_cast<int64_t>(engine.callers({*id}, 3, 0).nodes.size());
            std::printf("cucs_integration: [9] callers(data_batch::make) node_count at depth 1/2/3 = %lld/%lld/%lld\n",
                        static_cast<long long>(c1),
                        static_cast<long long>(c2),
                        static_cast<long long>(c3));
            check(c1 <= c2 && c2 <= c3, "[9] callers(data_batch::make) node_count is monotonically non-decreasing over depth 1..3");
        }
        else
        {
            check(false, "[9] callers(data_batch::make) node_count is monotonically non-decreasing over depth 1..3");
        }
    }

    // --- [10] CUDA XOR: kernel+launcher indexed with CALL edge XOR .cu file has an error row ---
    {
        // Names verified against the real
        // test/memory/test_gpu_kernels.cu as of writing this test (see the
        // Phase 6 report) -- two __global__ kernels and the two extern "C"
        // host launcher functions that call them via <<<>>>.
        struct Pair
        {
            std::string launcher;
            std::string kernel;
        };
        const std::vector<Pair> pairs = {
            {"performGpuMemoryWork", "gpu_memory_work_kernel"},
            {"verifyGpuMemoryWork", "gpu_memory_verification_kernel"},
        };

        bool allResolved = true;
        bool allHaveCallEdge = true;
        for (const auto& p : pairs)
        {
            const auto launcherId = resolveUnique(db, p.launcher, nullptr, "[10]/probe");
            const auto kernelId = resolveUnique(db, p.kernel, nullptr, "[10]/probe");
            if (!launcherId || !kernelId)
            {
                allResolved = false;
                continue;
            }
            const kg::query::BfsResult callees = engine.callees({*launcherId}, 1, 0);
            if (!containsNode(callees, *kernelId))
            {
                allHaveCallEdge = false;
            }
        }
        const bool kernelsIndexedWithCallEdge = allResolved && allHaveCallEdge;

        // Does the .cu file itself have an associated `error` row? (any
        // occurrence of an `error` element whose source_location resolves to
        // a `file` row whose path ends with the .cu file's name -- this is
        // how kg::index::SrctrldbBackend / the vendored
        // SourcetrailDBWriter::recordError ties an error to a location; the
        // `error.translation_unit` column itself is always empty -- the
        // vendored writer never populates it -- so it can't be used
        // directly here.)
        bool cuFileHasError = false;
        std::string cuFileErrorMessage;
        db.run(
            "SELECT e.message FROM error e "
            "JOIN occurrence o ON o.element_id = e.id "
            "JOIN source_location sl ON sl.id = o.source_location_id "
            "JOIN file f ON f.id = sl.file_node_id "
            "WHERE f.path LIKE '%test_gpu_kernels.cu'",
            [&](sqlite3_stmt* stmt) {
                cuFileHasError = true;
                const unsigned char* text = sqlite3_column_text(stmt, 0);
                cuFileErrorMessage = text ? reinterpret_cast<const char*>(text) : "";
            });

        std::printf(
            "cucs_integration: [10] kernelsIndexedWithCallEdge=%s, cuFileHasError=%s\n",
            kernelsIndexedWithCallEdge ? "true" : "false",
            cuFileHasError ? "true" : "false");
        if (cuFileHasError)
        {
            std::printf("cucs_integration: [10] .cu file error message: %s\n", cuFileErrorMessage.c_str());
        }

        // Surface which CUDA rung actually succeeded (parsed from -v stderr
        // output; see IndexDriver.cpp's per-TU progress line) -- a
        // nice-to-have introspection aid, not itself part of the JSON
        // schema (kg.index-summary/1 doesn't carry per-TU rung info today).
        std::istringstream stderrLines(stderrText);
        std::string line;
        while (std::getline(stderrLines, line))
        {
            if (line.find("test_gpu_kernels.cu:") != std::string::npos)
            {
                std::printf("cucs_integration: [10] index progress line: %s\n", line.c_str());
            }
        }

        check(
            kernelsIndexedWithCallEdge != cuFileHasError,
            "[10] CUDA XOR: exactly one of {kernel+launcher indexed with CALL edge, .cu file has an error row} holds");
    }

    std::printf("cucs_integration: SUMMARY %d passed, %d failed\n", g_pass, g_fail);
    return g_fail == 0 ? 0 : 1;
}
