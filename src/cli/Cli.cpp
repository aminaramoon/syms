#include "kg/cli/Cli.h"

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <exception>
#include <functional>
#include <iostream>
#include <memory>
#include <optional>
#include <string>
#include <unordered_set>
#include <vector>

#include <ctime>
#include <filesystem>

#include "kg/common/Process.h"
#include "kg/db/Database.h"
#include "kg/db/Enums.h"
#include "kg/db/NameDecode.h"
#include "kg/index/ArgSanitizer.h"
#include "kg/index/CompileDb.h"
#include "kg/index/CudaSupport.h"
#include "kg/index/FileRegistry.h"
#include "kg/index/IndexDriver.h"
#include "kg/index/Provenance.h"
#include "kg/out/JsonWriter.h"
#include "kg/out/TextWriter.h"
#include "kg/query/FuzzyMatcher.h"
#include "kg/query/QueryEngine.h"
#include "kg/query/SymbolResolver.h"
#include "kg/update/GitDiff.h"
#include "kg/update/Snapshot.h"
#include "kg/update/UpdateDriver.h"

#include <nlohmann/json.hpp>

#ifndef KG_VERSION_STRING
#define KG_VERSION_STRING "0.0.0"
#endif
#ifndef KG_LIBCLANG_VERSION
#define KG_LIBCLANG_VERSION "unknown"
#endif
#ifndef KG_SQLITE_VERSION
#define KG_SQLITE_VERSION "unknown"
#endif
#ifndef KG_SRCTRLDB_VERSION_STRING
#define KG_SRCTRLDB_VERSION_STRING "unknown"
#endif

namespace kg::cli
{

void printVersion()
{
    std::printf(
        "kg %s, libclang %s, sqlite %s, srctrldb %s\n",
        KG_VERSION_STRING,
        KG_LIBCLANG_VERSION,
        KG_SQLITE_VERSION,
        KG_SRCTRLDB_VERSION_STRING);
}

void printHelp(const char* programName)
{
    std::printf(
        "Usage: %s [OPTIONS]\n"
        "       %s index -c/--compile-commands PATH [OPTIONS]\n"
        "       %s query VERB SYMBOL-or-TEXT-or-PATH [OPTIONS]\n"
        "       %s update -d DB [OPTIONS]\n"
        "\n"
        "kg indexes C++/CUDA code via libclang into a Sourcetrail-schema-compatible\n"
        "SQLite database, and queries it.\n"
        "\n"
        "Options:\n"
        "  -h, --help       Show this help message and exit\n"
        "      --version    Show version information and exit\n"
        "\n"
        "index subcommand:\n"
        "  -c, --compile-commands PATH   compile_commands.json to load (required)\n"
        "  -o, --output PATH             output .srctrldb path (default: ./kg.srctrldb)\n"
        "      --force                   overwrite --output if it already exists\n"
        "      --repo DIR                git repo root for provenance metadata (default: discovered\n"
        "                                 via `git -C <dirname of -c PATH> rev-parse --show-toplevel`)\n"
        "      --remap OLD=NEW           literal substring path remap (repeatable)\n"
        "      --include GLOB            only process files matching GLOB (repeatable)\n"
        "      --exclude GLOB            never process files matching GLOB (repeatable)\n"
        "      --drop-arg PATTERN        drop any compiler arg token matching GLOB PATTERN (repeatable)\n"
        "      --extra-arg ARG           append ARG to every TU's argv, after all built-in\n"
        "                                 injections (repeatable, order preserved)\n"
        "      --resource-dir DIR        clang resource dir to inject (default: $KG_RESOURCE_DIR\n"
        "                                 or the build's own bundled resource dir)\n"
        "      --gcc-toolchain DIR       inject --gcc-toolchain=DIR (default: $KG_GCC_TOOLCHAIN)\n"
        "      --cuda-path DIR           CUDA toolkit root for CUDA rung synthesis (default:\n"
        "                                 $KG_CUDA_PATH, or derived from nvcc's path in the CDB)\n"
        "      --cuda-mode MODE          auto|full|nowrapper|host-only|skip (default: auto)\n"
        "      --system-file-rows MODE   none|direct|all: how many non-indexed files get a\n"
        "                                 `file` table row (default: direct)\n"
        "      --std STD                 fallback -std= value if none survives sanitizing\n"
        "                                 (default: c++17)\n"
        "      --limit N                 only process the first N (post-filter) TUs\n"
        "      --no-vacuum               skip the VACUUM step after indexing\n"
        "      --json                    also print a JSON summary object to stdout\n"
        "  -q, --quiet                   suppress the per-TU progress line\n"
        "      --dry-run                 print each TU's fully-sanitized argv and exit,\n"
        "                                 instead of actually indexing\n"
        "  -v, --verbose                 increase verbosity (repeatable)\n"
        "\n"
        "query subcommand:\n"
        "  VERB                          one of: callers callees bases derived overrides\n"
        "                                 overridden-by members neighbors references\n"
        "                                 definition search file stats\n"
        "  -d, --db PATH                 database to query (default: $KG_DB or ./kg.srctrldb)\n"
        "      --depth N                 BFS depth limit, 0 = unlimited (default: 1)\n"
        "      --max-nodes N             BFS node cap, 0 = unlimited (default: 500)\n"
        "      --kind K                  filter by node kind, e.g. CLASS (repeatable)\n"
        "      --edge-kind K             filter by edge kind, e.g. CALL (repeatable)\n"
        "      --location-kind K         filter `file` occurrences by location kind (repeatable)\n"
        "      --limit N                 result cap for search/stats --errors (default: 100)\n"
        "      --all-matches             if the symbol is ambiguous, query every candidate\n"
        "      --signature SIG           narrow symbol resolution by exact signature() match\n"
        "      --ensure-indices          create the reader-performance indices if missing\n"
        "      --contents                (file verb) include file contents\n"
        "      --occurrences             (file verb) include per-location occurrences\n"
        "      --lines A:B               (file verb) restrict occurrences to a line range\n"
        "      --errors N                (stats verb) how many error rows to show (default: 100)\n"
        "      --json                    print machine-readable JSON instead of text\n"
        "                                 (see docs/JSON_SCHEMA.md)\n"
        "  -v, --verbose                 increase verbosity (repeatable)\n"
        "\n"
        "update subcommand (incrementally re-index files changed since the last index/update):\n"
        "  kg update -d DB                 everything else is inferred from the provenance stored in DB\n"
        "  -d, --db PATH                   existing .srctrldb produced by this kg version (required)\n"
        "      --repo DIR                  git repo (default: stored at index time)\n"
        "      --to REV                    treat REV as the new baseline (default: HEAD; worktree must match)\n"
        "  -c, --compile-commands PATH     (default: stored path)\n"
        "      --files PATH...             explicit changed files instead of git\n"
        "      --remap/--include/--exclude/--cuda-mode/--system-file-rows/--extra-arg/--drop-arg/--std\n"
        "                                  override stored options (otherwise inherited)\n"
        "      --dry-run                   show what would be cleared/re-indexed, change nothing\n"
        "      --in-place                  modify DB directly instead of temp-copy + atomic rename\n"
        "      --no-vacuum, --json, -q, -v\n"
        "\n"
        "Exit codes: 0 ok, 1 usage error, 2 ambiguous symbol / refused (legacy db / update in progress),\n"
        "            3 not found, 4 database missing/unreadable/wrong version, 5 nothing to do (update).\n",
        programName,
        programName,
        programName,
        programName);
}

namespace
{

void printIndexUsageError(const std::string& message)
{
    std::fprintf(stderr, "kg index: %s\n", message.c_str());
    std::fprintf(stderr, "Try 'kg --help' for usage.\n");
}

// Parses `kg index` subcommand arguments (argv[startIdx..argc)) and either
// runs the --dry-run pipeline (load compile DB -> remap+filter (done inside
// CompileDb::load) -> sanitize -> print argv per TU) or the real
// libclang-backed indexing pipeline (kg::index::runIndex()).
std::string utcTimestampNow()
{
    const std::time_t t = std::time(nullptr);
    std::tm tm{};
    gmtime_r(&t, &tm);
    char buf[32];
    std::strftime(buf, sizeof(buf), "%Y-%m-%dT%H:%M:%SZ", &tm);
    return buf;
}

// Trims a trailing newline (git subcommands like `rev-parse` print one).
std::string trimTrailingNewline(std::string s)
{
    while (!s.empty() && (s.back() == '\n' || s.back() == '\r'))
    {
        s.pop_back();
    }
    return s;
}

// Resolves the git repo root/HEAD/dirty-paths for `cdbPath`'s provenance.
// If `repoDirFlag` is non-empty, that's used as the repo root directly
// (still validated via `git -C <dir> rev-parse --show-toplevel`); otherwise
// the parent directory of `cdbPath` is used as the starting point for git
// discovery. If git isn't on PATH or the directory isn't inside a repo at
// all, every field is left empty -- NOT an error (just a warning at -v).
kg::index::Provenance buildProvenance(
    const std::string& cdbPath, const std::string& repoDirFlag, const kg::index::IndexRunOptions& runOptions, int verbosity)
{
    kg::index::Provenance prov;
    prov.kgVersion = KG_VERSION_STRING;
    prov.walkerVersion = kg::index::kWalkerVersion;
    prov.cdbPath = std::filesystem::absolute(cdbPath).lexically_normal().string();
    prov.dbOptions = runOptions.dbOptions;
    prov.sanitizerOptions = runOptions.sanitizerOptions;
    prov.systemFileRows = runOptions.systemFileRows;
    prov.limit = runOptions.limit;
    prov.indexedAtUtc = utcTimestampNow();

    const std::string startDir =
        !repoDirFlag.empty() ? repoDirFlag : std::filesystem::absolute(cdbPath).parent_path().string();

    const kg::common::process::Result toplevel =
        kg::common::process::run({"git", "-C", startDir, "rev-parse", "--show-toplevel"});
    if (toplevel.exitCode != 0)
    {
        if (verbosity > 0)
        {
            std::fprintf(
                stderr,
                "kg index: warning: couldn't resolve a git repo root for provenance (from '%s'): %s\n",
                startDir.c_str(),
                trimTrailingNewline(toplevel.err).c_str());
        }
        return prov;
    }
    prov.repoRoot = trimTrailingNewline(toplevel.out);

    const kg::common::process::Result head =
        kg::common::process::run({"git", "-C", prov.repoRoot, "rev-parse", "HEAD"});
    if (head.exitCode == 0)
    {
        prov.repoCommit = trimTrailingNewline(head.out);
    }
    else if (verbosity > 0)
    {
        std::fprintf(stderr, "kg index: warning: couldn't resolve HEAD for provenance: %s\n", trimTrailingNewline(head.err).c_str());
    }

    const kg::common::process::Result status =
        kg::common::process::run({"git", "-C", prov.repoRoot, "status", "--porcelain", "-z", "--untracked-files=all"});
    if (status.exitCode == 0)
    {
        // Delegate to kg::update::parseStatusPorcelainZ (kg_update, which
        // the `kg` executable already links -- see src/CMakeLists.txt)
        // rather than re-parsing "XY path\0[old-path\0]" records inline
        // here: it already handles renames AND copies (X or Y == 'R'/'C'
        // consumes the extra old-path token), which the old inline version
        // missed for plain copy ('C') records.
        prov.dirtyPaths = kg::update::parseStatusPorcelainZ(status.out, prov.repoRoot);
    }
    else if (verbosity > 0)
    {
        std::fprintf(stderr, "kg index: warning: couldn't resolve dirty paths for provenance: %s\n", trimTrailingNewline(status.err).c_str());
    }

    return prov;
}

int runIndex(int argc, char** argv, int startIdx)
{
    std::string compileCommandsPath;
    std::string outputPath = "./kg.srctrldb";
    kg::index::CompileDbOptions dbOptions;
    kg::index::SanitizerOptions sanitizerOptions;

    bool dryRun = false;
    bool force = false;
    bool quiet = false;
    bool jsonSummary = false;
    bool noVacuum = false;
    kg::index::SystemFileRows systemFileRows = kg::index::SystemFileRows::Direct;
    int verbosity = 0;
    long limit = -1;  // -1 = no limit
    bool resourceDirGiven = false;
    bool cudaPathGiven = false;
    bool gccToolchainGiven = false;
    std::string repoDir;

    auto requireValue = [&](int i, const std::string& flagName) -> std::string {
        if (i + 1 >= argc)
        {
            printIndexUsageError("option '" + flagName + "' requires a value");
            std::exit(2);
        }
        return argv[i + 1];
    };

    for (int i = startIdx; i < argc; ++i)
    {
        const std::string arg = argv[i];

        if (arg == "-c" || arg == "--compile-commands")
        {
            compileCommandsPath = requireValue(i, arg);
            ++i;
        }
        else if (arg == "-o" || arg == "--output")
        {
            outputPath = requireValue(i, arg);
            ++i;
        }
        else if (arg == "--remap")
        {
            const std::string value = requireValue(i, arg);
            ++i;
            try
            {
                dbOptions.remaps.push_back(kg::index::parseRemapArg(value));
            }
            catch (const std::exception& e)
            {
                printIndexUsageError(e.what());
                return 2;
            }
        }
        else if (arg == "--include")
        {
            dbOptions.includeGlobs.push_back(requireValue(i, arg));
            ++i;
        }
        else if (arg == "--exclude")
        {
            dbOptions.excludeGlobs.push_back(requireValue(i, arg));
            ++i;
        }
        else if (arg == "--drop-arg")
        {
            sanitizerOptions.dropArgPatterns.push_back(requireValue(i, arg));
            ++i;
        }
        else if (arg == "--extra-arg")
        {
            sanitizerOptions.extraArgs.push_back(requireValue(i, arg));
            ++i;
        }
        else if (arg == "--resource-dir")
        {
            sanitizerOptions.resourceDir = requireValue(i, arg);
            resourceDirGiven = true;
            ++i;
        }
        else if (arg == "--gcc-toolchain")
        {
            sanitizerOptions.gccToolchain = requireValue(i, arg);
            gccToolchainGiven = true;
            ++i;
        }
        else if (arg == "--force")
        {
            force = true;
        }
        else if (arg == "--repo")
        {
            repoDir = requireValue(i, arg);
            ++i;
        }
        else if (arg == "--system-file-rows")
        {
            const std::string value = requireValue(i, arg);
            ++i;
            try
            {
                systemFileRows = kg::index::parseSystemFileRows(value);
            }
            catch (const std::exception& e)
            {
                printIndexUsageError(e.what());
                return 2;
            }
        }
        else if (arg == "--no-vacuum")
        {
            noVacuum = true;
        }
        else if (arg == "--json")
        {
            jsonSummary = true;
        }
        else if (arg == "-q" || arg == "--quiet")
        {
            quiet = true;
        }
        else if (arg == "--cuda-path")
        {
            sanitizerOptions.cudaPath = requireValue(i, arg);
            cudaPathGiven = true;
            ++i;
        }
        else if (arg == "--cuda-mode")
        {
            const std::string value = requireValue(i, arg);
            ++i;
            try
            {
                sanitizerOptions.cudaMode = kg::index::cuda::parseCudaMode(value);
            }
            catch (const std::exception& e)
            {
                printIndexUsageError(e.what());
                return 2;
            }
        }
        else if (arg == "--std")
        {
            sanitizerOptions.stdFallback = requireValue(i, arg);
            ++i;
        }
        else if (arg == "--limit")
        {
            const std::string value = requireValue(i, arg);
            ++i;
            try
            {
                limit = std::stol(value);
            }
            catch (const std::exception&)
            {
                printIndexUsageError("invalid --limit value: " + value);
                return 2;
            }
        }
        else if (arg == "--dry-run")
        {
            dryRun = true;
        }
        else if (arg == "-v" || arg == "--verbose")
        {
            ++verbosity;
        }
        else if (arg == "-h" || arg == "--help")
        {
            printHelp("kg");
            return 0;
        }
        else
        {
            printIndexUsageError("unknown option '" + arg + "'");
            return 2;
        }
    }
    if (compileCommandsPath.empty())
    {
        printIndexUsageError("missing required option -c/--compile-commands PATH");
        return 2;
    }

    // Resolve env-var fallbacks for flags not given explicitly on the CLI.
    // (The compile-time KG_DEFAULT_RESOURCE_DIR macro fallback for
    // --resource-dir is resolved inside ArgSanitizer.cpp itself -- see the
    // comment on SanitizerOptions::resourceDir in ArgSanitizer.h.)
    if (!resourceDirGiven)
    {
        if (const char* env = std::getenv("KG_RESOURCE_DIR"))
        {
            sanitizerOptions.resourceDir = env;
        }
    }
    if (!cudaPathGiven)
    {
        if (const char* env = std::getenv("KG_CUDA_PATH"))
        {
            sanitizerOptions.cudaPath = env;
        }
    }
    if (!gccToolchainGiven)
    {
        if (const char* env = std::getenv("KG_GCC_TOOLCHAIN"))
        {
            sanitizerOptions.gccToolchain = env;
        }
    }

    if (dryRun)
    {
        std::vector<kg::index::CompileEntry> entries;
        try
        {
            entries = kg::index::load(compileCommandsPath, dbOptions);
        }
        catch (const std::exception& e)
        {
            std::fprintf(stderr, "kg index: %s\n", e.what());
            return 1;
        }

        if (limit >= 0 && static_cast<std::size_t>(limit) < entries.size())
        {
            entries.resize(static_cast<std::size_t>(limit));
        }

        for (const auto& entry : entries)
        {
            kg::index::SanitizeResult result;
            try
            {
                result = kg::index::sanitize(entry, sanitizerOptions);
            }
            catch (const std::exception& e)
            {
                std::fprintf(stderr, "kg index: failed to sanitize '%s': %s\n", entry.file.c_str(), e.what());
                continue;
            }

            if (result.skipped)
            {
                std::printf("%s: SKIPPED (--cuda-mode skip)\n", entry.file.c_str());
                continue;
            }

            std::printf("%s:", entry.file.c_str());
            for (const auto& tok : result.args)
            {
                std::printf(" %s", tok.c_str());
            }
            std::printf("\n");
        }

        return 0;
    }

    kg::index::IndexRunOptions runOptions;
    runOptions.compileCommandsPath = compileCommandsPath;
    runOptions.outputPath = outputPath;
    runOptions.force = force;
    runOptions.limit = limit;
    runOptions.quiet = quiet;
    runOptions.jsonSummary = jsonSummary;
    runOptions.noVacuum = noVacuum;
    runOptions.verbose = verbosity > 0;
    runOptions.dbOptions = dbOptions;
    runOptions.sanitizerOptions = sanitizerOptions;
    runOptions.systemFileRows = systemFileRows;
    runOptions.kgVersion = KG_VERSION_STRING;

    // Provenance is always built for `kg index` (Phase 1 groundwork for a
    // future `kg update`) -- an unresolvable git repo just leaves the repo
    // fields empty, it's never a hard error.
    runOptions.provenance = buildProvenance(compileCommandsPath, repoDir, runOptions, verbosity);

    return kg::index::runIndex(runOptions);
}

void printQueryUsageError(const std::string& message)
{
    std::fprintf(stderr, "kg query: %s\n", message.c_str());
    std::fprintf(stderr, "Try 'kg --help' for usage.\n");
}

bool tryParseInt(const std::string& s, int& out)
{
    if (s.empty())
    {
        return false;
    }
    try
    {
        std::size_t pos = 0;
        out = std::stoi(s, &pos);
        return pos == s.size();
    }
    catch (const std::exception&)
    {
        return false;
    }
}

struct QueryFlags
{
    std::string dbPath;
    int depth = 1;
    int maxNodes = 500;
    int limit = 100;
    std::vector<std::string> kindNames;
    std::vector<std::string> edgeKindNames;
    std::vector<std::string> locationKindNames;
    bool allMatches = false;
    bool hasSignature = false;
    std::string signature;
    bool ensureIndices = false;
    bool wantContents = false;
    bool wantOccurrences = false;
    bool hasLineRange = false;
    int lineA = 0;
    int lineB = 0;
    int errorsLimit = 100;
    int verbosity = 0;
    bool json = false;
};

// Parses everything after the verb into `flags` and (if present) the single
// SYMBOL/TEXT/PATH positional argument. Returns false (having already
// printed a usage error) on any parse failure.
bool parseQueryFlags(int argc, char** argv, int startIdx, QueryFlags& flags, std::string& positional, bool& havePositional)
{
    havePositional = false;

    auto requireValue = [&](int i, const std::string& flagName) -> std::string {
        if (i + 1 >= argc)
        {
            printQueryUsageError("option '" + flagName + "' requires a value");
            std::exit(1);
        }
        return argv[i + 1];
    };
    auto requireIntValue = [&](int i, const std::string& flagName, int& out) -> bool {
        const std::string v = requireValue(i, flagName);
        if (!tryParseInt(v, out))
        {
            printQueryUsageError("option '" + flagName + "' expects an integer, got '" + v + "'");
            return false;
        }
        return true;
    };

    for (int i = startIdx; i < argc; ++i)
    {
        const std::string arg = argv[i];
        if (arg == "-d" || arg == "--db")
        {
            flags.dbPath = requireValue(i, arg);
            ++i;
        }
        else if (arg == "--depth")
        {
            if (!requireIntValue(i, arg, flags.depth))
            {
                return false;
            }
            ++i;
        }
        else if (arg == "--max-nodes")
        {
            if (!requireIntValue(i, arg, flags.maxNodes))
            {
                return false;
            }
            ++i;
        }
        else if (arg == "--kind")
        {
            flags.kindNames.push_back(requireValue(i, arg));
            ++i;
        }
        else if (arg == "--edge-kind")
        {
            flags.edgeKindNames.push_back(requireValue(i, arg));
            ++i;
        }
        else if (arg == "--location-kind")
        {
            flags.locationKindNames.push_back(requireValue(i, arg));
            ++i;
        }
        else if (arg == "--limit")
        {
            if (!requireIntValue(i, arg, flags.limit))
            {
                return false;
            }
            ++i;
        }
        else if (arg == "--all-matches")
        {
            flags.allMatches = true;
        }
        else if (arg == "--signature")
        {
            flags.signature = requireValue(i, arg);
            flags.hasSignature = true;
            ++i;
        }
        else if (arg == "--ensure-indices")
        {
            flags.ensureIndices = true;
        }
        else if (arg == "--contents")
        {
            flags.wantContents = true;
        }
        else if (arg == "--occurrences")
        {
            flags.wantOccurrences = true;
        }
        else if (arg == "--lines")
        {
            const std::string v = requireValue(i, arg);
            ++i;
            const std::string::size_type colon = v.find(':');
            bool ok = colon != std::string::npos;
            if (ok)
            {
                ok = tryParseInt(v.substr(0, colon), flags.lineA) && tryParseInt(v.substr(colon + 1), flags.lineB);
            }
            if (!ok)
            {
                printQueryUsageError("invalid --lines value '" + v + "' (expected A:B)");
                return false;
            }
            flags.hasLineRange = true;
        }
        else if (arg == "--errors")
        {
            if (!requireIntValue(i, arg, flags.errorsLimit))
            {
                return false;
            }
            ++i;
        }
        else if (arg == "--json")
        {
            flags.json = true;
        }
        else if (arg == "-v" || arg == "--verbose")
        {
            ++flags.verbosity;
        }
        else if (arg == "-h" || arg == "--help")
        {
            printHelp("kg");
            std::exit(0);
        }
        else if (!arg.empty() && arg.front() == '-')
        {
            printQueryUsageError("unknown option '" + arg + "'");
            return false;
        }
        else
        {
            if (havePositional)
            {
                printQueryUsageError("unexpected extra argument '" + arg + "'");
                return false;
            }
            positional = arg;
            havePositional = true;
        }
    }
    return true;
}

// Resolves `names` (raw --kind/--edge-kind/--location-kind flag values)
// through `fromString`, appending to `out`. Prints a usage error and returns
// false on the first unrecognized name.
bool resolveKindNames(
    const std::vector<std::string>& names,
    const std::function<std::optional<int>(const std::string&)>& fromString,
    const char* flagLabel,
    std::vector<int>& out)
{
    for (const std::string& name : names)
    {
        const std::optional<int> value = fromString(name);
        if (!value.has_value())
        {
            printQueryUsageError(std::string("unknown ") + flagLabel + " value '" + name + "'");
            return false;
        }
        out.push_back(*value);
    }
    return true;
}

std::vector<kg::out::CandidateRow> toCandidateRows(kg::query::QueryEngine& engine, const std::vector<kg::query::ResolvedSymbol>& candidates)
{
    std::vector<kg::out::CandidateRow> rows;
    rows.reserve(candidates.size());
    for (const kg::query::ResolvedSymbol& c : candidates)
    {
        kg::out::CandidateRow row;
        row.id = c.id;
        row.kind = kg::db::nodeKindToString(c.kind);
        row.name = kg::db::decodeSerializedName(c.serializedName).displayName();
        const kg::query::DefinitionResult def = engine.definition({c.id});
        if (!def.locations.empty())
        {
            row.location = def.locations.front().filePath + ":" + std::to_string(def.locations.front().startLine);
        }
        rows.push_back(std::move(row));
    }
    return rows;
}

// Wall-clock elapsed milliseconds since `t0`, for the JSON envelope's
// stats.elapsed_ms field (see docs/JSON_SCHEMA.md). Text output doesn't need
// this.
int64_t millisSince(std::chrono::steady_clock::time_point t0)
{
    return static_cast<int64_t>(std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::steady_clock::now() - t0).count());
}

// Resolves `spec` to a set of BFS seeds per the CLI's ambiguous/not-found
// contract. On success (Unique, or Ambiguous+--all-matches) populates
// `seeds` and returns -1 ("proceed"); otherwise prints the appropriate
// message/table (or, under --json, the kg.ambiguous/1 / kg.not-found/1
// envelope to stdout -- see docs/JSON_SCHEMA.md) and returns the exit code
// the caller should return immediately (2 for ambiguous, 3 for not found).
// `jsonWriter`/`jsonCtx` are only dereferenced when `jsonMode` is true.
int resolveSeedsOrReport(
    kg::db::Database& db,
    kg::query::QueryEngine& engine,
    const std::string& spec,
    const std::vector<int>* kindFilter,
    const std::string* signatureFilter,
    bool allMatches,
    std::vector<int64_t>& seeds,
    bool jsonMode,
    const kg::out::json::JsonWriter* jsonWriter,
    const kg::out::json::QueryContext* jsonCtx)
{
    const kg::query::ResolveResult rr = kg::query::resolveSymbol(db, spec, kindFilter, signatureFilter);

    if (rr.outcome == kg::query::ResolveOutcome::Unique)
    {
        seeds.push_back(rr.candidates.front().id);
        return -1;
    }

    if (rr.outcome == kg::query::ResolveOutcome::Ambiguous)
    {
        if (allMatches)
        {
            for (const kg::query::ResolvedSymbol& c : rr.candidates)
            {
                seeds.push_back(c.id);
            }
            return -1;
        }
        if (jsonMode)
        {
            std::cout << jsonWriter->ambiguous(*jsonCtx, rr.candidates).dump() << "\n";
        }
        else
        {
            std::printf("kg query: '%s' is ambiguous (%zu candidates):\n", spec.c_str(), rr.candidates.size());
            kg::out::writeCandidateTable(std::cout, toCandidateRows(engine, rr.candidates));
            std::printf("Pass --all-matches to query every candidate, or use '#<id>' to pick one directly.\n");
        }
        return 2;
    }

    // NotFound: fuzzy-suggest against every node's display name.
    const std::vector<kg::query::SearchHit> hits = engine.search(spec, nullptr, 5);
    if (jsonMode)
    {
        std::cout << jsonWriter->notFound(*jsonCtx, hits).dump() << "\n";
    }
    else
    {
        std::fprintf(stderr, "kg query: symbol '%s' not found.\n", spec.c_str());
        if (!hits.empty())
        {
            std::fprintf(stderr, "did you mean:\n");
            for (const kg::query::SearchHit& h : hits)
            {
                std::fprintf(
                    stderr, "  %s (#%lld, %s)\n", h.displayName.c_str(), static_cast<long long>(h.nodeId), kg::db::nodeKindToString(h.kind).c_str());
            }
        }
    }
    return 3;
}

int edgeKindMaskOr(const std::vector<int>& kinds, int fallback)
{
    if (kinds.empty())
    {
        return fallback;
    }
    int mask = 0;
    for (int k : kinds)
    {
        mask |= k;
    }
    return mask;
}

// Parses `kg query` subcommand arguments (argv[startIdx..argc)) and
// dispatches to kg::query::QueryEngine, rendering results via
// kg::out::TextWriter. See printHelp() for the flag/exit-code contract.
int runQuery(int argc, char** argv, int startIdx)
{
    static const std::unordered_set<std::string> kKnownVerbs = {
        "callers",
        "callees",
        "bases",
        "derived",
        "overrides",
        "overridden-by",
        "members",
        "neighbors",
        "references",
        "definition",
        "search",
        "file",
        "stats",
    };

    if (startIdx >= argc)
    {
        printQueryUsageError("missing VERB (expected one of: callers callees bases derived overrides overridden-by "
                              "members neighbors references definition search file stats)");
        return 1;
    }

    const std::string verb = argv[startIdx];
    if (verb == "-h" || verb == "--help")
    {
        printHelp("kg");
        return 0;
    }
    if (kKnownVerbs.find(verb) == kKnownVerbs.end())
    {
        printQueryUsageError("unknown verb '" + verb + "'");
        return 1;
    }

    QueryFlags flags;
    std::string positional;
    bool havePositional = false;
    if (!parseQueryFlags(argc, argv, startIdx + 1, flags, positional, havePositional))
    {
        return 1;
    }

    const bool needsPositional = verb != "stats";
    if (needsPositional && !havePositional)
    {
        printQueryUsageError("verb '" + verb + "' requires a SYMBOL/TEXT/PATH argument");
        return 1;
    }
    if (!needsPositional && havePositional)
    {
        printQueryUsageError("verb 'stats' does not take a positional argument");
        return 1;
    }

    if (flags.dbPath.empty())
    {
        if (const char* env = std::getenv("KG_DB"))
        {
            flags.dbPath = env;
        }
        else
        {
            flags.dbPath = "./kg.srctrldb";
        }
    }

    std::vector<int> kindFilter;
    std::vector<int> edgeKindFilter;
    std::vector<int> locationKindFilter;
    if (!resolveKindNames(flags.kindNames, kg::db::nodeKindFromString, "--kind", kindFilter) ||
        !resolveKindNames(flags.edgeKindNames, kg::db::edgeKindFromString, "--edge-kind", edgeKindFilter) ||
        !resolveKindNames(flags.locationKindNames, kg::db::locationKindFromString, "--location-kind", locationKindFilter))
    {
        return 1;
    }

    std::unique_ptr<kg::db::Database> dbPtr;
    try
    {
        if (flags.ensureIndices)
        {
            kg::db::ensureIndices(flags.dbPath);
        }
        dbPtr = std::make_unique<kg::db::Database>(flags.dbPath, flags.verbosity);
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "kg query: %s\n", e.what());
        return 4;
    }

    try
    {
        kg::db::Database& db = *dbPtr;
        kg::query::QueryEngine engine(db);

        const int nodeKindMask = edgeKindMaskOr(kindFilter, 0);

        std::unique_ptr<kg::out::json::JsonWriter> jsonWriter;
        if (flags.json)
        {
            jsonWriter = std::make_unique<kg::out::json::JsonWriter>(db, engine, KG_VERSION_STRING);
        }

        auto renderBfs = [&](const kg::query::BfsResult& r) {
            std::vector<int64_t> ids;
            ids.reserve(r.nodes.size());
            for (const kg::query::BfsNode& n : r.nodes)
            {
                ids.push_back(n.id);
            }
            const auto locs = engine.primaryLocations(ids);
            kg::out::writeBfsResult(std::cout, r, locs, flags.verbosity > 0);
        };

        if (verb == "search")
        {
            const auto t0 = std::chrono::steady_clock::now();
            const std::vector<kg::query::SearchHit> hits = engine.search(positional, kindFilter.empty() ? nullptr : &kindFilter, flags.limit);
            const int64_t elapsedMs = millisSince(t0);
            if (flags.json)
            {
                kg::out::json::QueryContext ctx;
                ctx.verb = verb;
                ctx.db = flags.dbPath;
                ctx.symbolOrTextOrPath = positional;
                ctx.limit = flags.limit;
                ctx.searchKinds = kindFilter;
                std::cout << jsonWriter->search(ctx, hits, elapsedMs).dump() << "\n";
            }
            else
            {
                kg::out::writeSearchHits(std::cout, hits);
            }
            return 0;
        }

        if (verb == "stats")
        {
            const kg::query::StatsResult sr = engine.stats(flags.errorsLimit);
            if (flags.json)
            {
                kg::out::json::QueryContext ctx;
                ctx.verb = verb;
                ctx.db = flags.dbPath;
                std::cout << jsonWriter->stats(ctx, sr).dump() << "\n";
            }
            else
            {
                kg::out::writeStats(std::cout, sr);
            }
            return 0;
        }

        if (verb == "file")
        {
            const std::optional<std::pair<int, int>> lineRange =
                flags.hasLineRange ? std::optional<std::pair<int, int>>({flags.lineA, flags.lineB}) : std::nullopt;
            const std::vector<int>* locFilter = locationKindFilter.empty() ? nullptr : &locationKindFilter;

            kg::out::json::QueryContext jsonCtx;
            if (flags.json)
            {
                jsonCtx.verb = verb;
                jsonCtx.db = flags.dbPath;
                jsonCtx.symbolOrTextOrPath = positional;
            }

            const kg::query::ResolveResult rr = kg::query::resolveFilePath(db, positional);
            if (rr.outcome == kg::query::ResolveOutcome::NotFound)
            {
                const std::vector<std::pair<int64_t, std::string>> candidates(engine.fileMap().begin(), engine.fileMap().end());
                const std::vector<kg::query::FuzzyHit> hits = kg::query::fuzzyMatch(positional, candidates, 5);
                if (flags.json)
                {
                    std::cout << jsonWriter->notFoundFiles(jsonCtx, hits, engine.fileMap()).dump() << "\n";
                }
                else
                {
                    std::fprintf(stderr, "kg query: file '%s' not found.\n", positional.c_str());
                    if (!hits.empty())
                    {
                        std::fprintf(stderr, "did you mean:\n");
                        for (const kg::query::FuzzyHit& h : hits)
                        {
                            const auto it = engine.fileMap().find(h.nodeId);
                            std::fprintf(stderr, "  %s\n", it != engine.fileMap().end() ? it->second.c_str() : "?");
                        }
                    }
                }
                return 3;
            }
            if (rr.outcome == kg::query::ResolveOutcome::Ambiguous && !flags.allMatches)
            {
                if (flags.json)
                {
                    std::cout << jsonWriter->ambiguous(jsonCtx, rr.candidates).dump() << "\n";
                }
                else
                {
                    std::printf("kg query: '%s' matches multiple files:\n", positional.c_str());
                    std::vector<kg::out::CandidateRow> rows;
                    for (const kg::query::ResolvedSymbol& c : rr.candidates)
                    {
                        rows.push_back(kg::out::CandidateRow{c.id, "FILE", c.serializedName, ""});
                    }
                    kg::out::writeCandidateTable(std::cout, rows);
                    std::printf("Pass --all-matches to show every candidate.\n");
                }
                return 2;
            }
            for (const kg::query::ResolvedSymbol& c : rr.candidates)
            {
                const kg::query::FileResult fr = engine.fileInfo(c.id, flags.wantContents, flags.wantOccurrences, lineRange, locFilter);
                if (flags.json)
                {
                    std::cout << jsonWriter->file(jsonCtx, fr, flags.wantContents, flags.wantOccurrences).dump() << "\n";
                }
                else
                {
                    kg::out::writeFileResult(std::cout, fr);
                    std::cout << "\n";
                }
            }
            return 0;
        }

        // Every remaining verb takes a resolved symbol.
        std::vector<int64_t> seeds;
        kg::out::json::QueryContext jsonCtx;
        if (flags.json)
        {
            jsonCtx.verb = verb;
            jsonCtx.db = flags.dbPath;
            jsonCtx.symbolOrTextOrPath = positional;
        }
        {
            const std::vector<int>* kf = kindFilter.empty() ? nullptr : &kindFilter;
            const std::string* sig = flags.hasSignature ? &flags.signature : nullptr;
            const int rc =
                resolveSeedsOrReport(db, engine, positional, kf, sig, flags.allMatches, seeds, flags.json, jsonWriter.get(), &jsonCtx);
            if (rc >= 0)
            {
                return rc;
            }
        }
        jsonCtx.symbolIds = seeds;
        jsonCtx.nodeKinds = kindFilter;

        // Emits either the text tree or the kg.graph/1 envelope for a graph
        // verb's already-computed BfsResult. `depth`/`maxNodes`/`edgeKinds`
        // are what the verb ACTUALLY applied -- overrides/overridden-by/
        // members/neighbors ignore --depth/--max-nodes entirely (fixed at
        // depth=1, unlimited nodes), so those are reported as the real fixed
        // values those verbs use, not flags.depth/flags.maxNodes verbatim.
        auto emitGraph = [&](const kg::query::BfsResult& r, int depth, int maxNodes, std::vector<int> edgeKinds, int64_t elapsedMs) {
            if (flags.json)
            {
                jsonCtx.depth = depth;
                jsonCtx.maxNodes = maxNodes;
                jsonCtx.edgeKinds = std::move(edgeKinds);
                std::cout << jsonWriter->graph(jsonCtx, r, elapsedMs).dump() << "\n";
            }
            else
            {
                renderBfs(r);
            }
        };

        if (verb == "callers")
        {
            const auto t0 = std::chrono::steady_clock::now();
            const kg::query::BfsResult r = engine.callers(seeds, flags.depth, flags.maxNodes, nodeKindMask);
            emitGraph(r, flags.depth, flags.maxNodes, {kg::db::EdgeKind::CALL}, millisSince(t0));
        }
        else if (verb == "callees")
        {
            const auto t0 = std::chrono::steady_clock::now();
            const kg::query::BfsResult r = engine.callees(seeds, flags.depth, flags.maxNodes, nodeKindMask);
            emitGraph(r, flags.depth, flags.maxNodes, {kg::db::EdgeKind::CALL}, millisSince(t0));
        }
        else if (verb == "bases")
        {
            const auto t0 = std::chrono::steady_clock::now();
            const kg::query::BfsResult r = engine.bases(seeds, flags.depth, flags.maxNodes, nodeKindMask);
            emitGraph(r, flags.depth, flags.maxNodes, {kg::db::EdgeKind::INHERITANCE}, millisSince(t0));
        }
        else if (verb == "derived")
        {
            const auto t0 = std::chrono::steady_clock::now();
            const kg::query::BfsResult r = engine.derived(seeds, flags.depth, flags.maxNodes, nodeKindMask);
            emitGraph(r, flags.depth, flags.maxNodes, {kg::db::EdgeKind::INHERITANCE}, millisSince(t0));
        }
        else if (verb == "overrides")
        {
            const auto t0 = std::chrono::steady_clock::now();
            const kg::query::BfsResult r = engine.overrides(seeds, nodeKindMask);
            emitGraph(r, /*depth=*/1, /*maxNodes=*/0, {kg::db::EdgeKind::OVERRIDE}, millisSince(t0));
        }
        else if (verb == "overridden-by")
        {
            const auto t0 = std::chrono::steady_clock::now();
            const kg::query::BfsResult r = engine.overriddenBy(seeds, nodeKindMask);
            emitGraph(r, /*depth=*/1, /*maxNodes=*/0, {kg::db::EdgeKind::OVERRIDE}, millisSince(t0));
        }
        else if (verb == "members")
        {
            const auto t0 = std::chrono::steady_clock::now();
            const kg::query::BfsResult r = engine.members(seeds, nodeKindMask);
            emitGraph(r, /*depth=*/1, /*maxNodes=*/0, {kg::db::EdgeKind::MEMBER}, millisSince(t0));
        }
        else if (verb == "neighbors")
        {
            const int edgeMask = edgeKindMaskOr(edgeKindFilter, kg::db::EdgeKind::ALL);
            const auto t0 = std::chrono::steady_clock::now();
            const kg::query::BfsResult r = engine.neighbors(seeds, edgeMask);
            const std::vector<int> edgeKinds = edgeKindFilter.empty() ? kg::out::json::decomposeEdgeKindMask(edgeMask) : edgeKindFilter;
            emitGraph(r, /*depth=*/1, /*maxNodes=*/0, edgeKinds, millisSince(t0));
        }
        else if (verb == "references")
        {
            const auto t0 = std::chrono::steady_clock::now();
            const kg::query::ReferencesResult r = engine.references(seeds);
            const int64_t elapsedMs = millisSince(t0);
            if (flags.json)
            {
                std::cout << jsonWriter->references(jsonCtx, r, elapsedMs).dump() << "\n";
            }
            else
            {
                kg::out::writeReferences(std::cout, r);
            }
        }
        else if (verb == "definition")
        {
            const auto t0 = std::chrono::steady_clock::now();
            const kg::query::DefinitionResult r = engine.definition(seeds);
            const int64_t elapsedMs = millisSince(t0);
            if (flags.json)
            {
                std::cout << jsonWriter->definition(jsonCtx, r, elapsedMs).dump() << "\n";
            }
            else
            {
                kg::out::writeDefinition(std::cout, r);
            }
        }
        else
        {
            printQueryUsageError("unhandled verb '" + verb + "'");
            return 1;
        }
        return 0;
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "kg query: error: %s\n", e.what());
        return 1;
    }
}

void printUpdateUsageError(const std::string& message)
{
    std::fprintf(stderr, "kg update: %s\n", message.c_str());
    std::fprintf(stderr, "Try 'kg --help' for usage.\n");
}

// Parses `kg update` subcommand arguments (argv[startIdx..argc)) into a
// kg::update::UpdateRunOptions. --files is repeatable AND greedily consumes
// every following non-flag positional too (so `--files a.cpp b.cpp` works
// without repeating the flag), stopping at the next `-`-prefixed token.
bool parseUpdateFlags(int argc, char** argv, int startIdx, kg::update::UpdateRunOptions& opts)
{
    // Exit 1 (usage error), NOT 2 -- 2 is documented as "ambiguous symbol /
    // refused (legacy db / update in progress)" (see printHelp()'s exit
    // codes list), a semantically different failure from "you typo'd a
    // flag". Matches every other subcommand's own requireValue (e.g. the
    // `kg query` one a few hundred lines up) already exiting 1 here.
    auto requireValue = [&](int i, const std::string& flagName) -> std::string {
        if (i + 1 >= argc)
        {
            printUpdateUsageError("option '" + flagName + "' requires a value");
            std::exit(1);
        }
        return argv[i + 1];
    };

    for (int i = startIdx; i < argc; ++i)
    {
        const std::string arg = argv[i];
        if (arg == "-d" || arg == "--db")
        {
            opts.dbPath = requireValue(i, arg);
            ++i;
        }
        else if (arg == "--repo")
        {
            opts.repoRoot = requireValue(i, arg);
            ++i;
        }
        else if (arg == "--to")
        {
            opts.toRev = requireValue(i, arg);
            ++i;
        }
        else if (arg == "--from")
        {
            opts.fromRev = requireValue(i, arg);
            ++i;
        }
        else if (arg == "-c" || arg == "--compile-commands")
        {
            opts.cdbPath = requireValue(i, arg);
            ++i;
        }
        else if (arg == "--files")
        {
            ++i;
            if (i >= argc)
            {
                printUpdateUsageError("option '--files' requires at least one value");
                return false;
            }
            for (; i < argc && !(std::string(argv[i]).size() > 1 && argv[i][0] == '-'); ++i)
            {
                opts.explicitFiles.push_back(argv[i]);
            }
            --i;
        }
        else if (arg == "--remap")
        {
            const std::string value = requireValue(i, arg);
            ++i;
            try
            {
                opts.dbOverrides.remaps.push_back(kg::index::parseRemapArg(value));
            }
            catch (const std::exception& e)
            {
                printUpdateUsageError(e.what());
                return false;
            }
            opts.dbOverridesGiven = true;
        }
        else if (arg == "--include")
        {
            opts.dbOverrides.includeGlobs.push_back(requireValue(i, arg));
            ++i;
            opts.dbOverridesGiven = true;
        }
        else if (arg == "--exclude")
        {
            opts.dbOverrides.excludeGlobs.push_back(requireValue(i, arg));
            ++i;
            opts.dbOverridesGiven = true;
        }
        else if (arg == "--cuda-mode")
        {
            const std::string value = requireValue(i, arg);
            ++i;
            try
            {
                opts.cudaModeOverride = kg::index::cuda::parseCudaMode(value);
            }
            catch (const std::exception& e)
            {
                printUpdateUsageError(e.what());
                return false;
            }
            opts.cudaModeGiven = true;
        }
        else if (arg == "--system-file-rows")
        {
            const std::string value = requireValue(i, arg);
            ++i;
            try
            {
                opts.systemFileRowsOverride = kg::index::parseSystemFileRows(value);
            }
            catch (const std::exception& e)
            {
                printUpdateUsageError(e.what());
                return false;
            }
            opts.systemFileRowsGiven = true;
        }
        else if (arg == "--extra-arg")
        {
            opts.extraArgOverride.push_back(requireValue(i, arg));
            ++i;
            opts.extraArgGiven = true;
        }
        else if (arg == "--drop-arg")
        {
            opts.dropArgOverride.push_back(requireValue(i, arg));
            ++i;
            opts.dropArgGiven = true;
        }
        else if (arg == "--std")
        {
            opts.stdOverride = requireValue(i, arg);
            ++i;
            opts.stdGiven = true;
        }
        else if (arg == "--dry-run")
        {
            opts.dryRun = true;
        }
        else if (arg == "--in-place")
        {
            opts.inPlace = true;
        }
        else if (arg == "--no-vacuum")
        {
            opts.noVacuum = true;
        }
        else if (arg == "--json")
        {
            opts.json = true;
        }
        else if (arg == "-q" || arg == "--quiet")
        {
            // Quiet is already the default (runUpdate only prints per-TU
            // progress lines when verbose > 0) -- accepted for symmetry
            // with `kg index`/`kg query`, but a no-op.
        }
        else if (arg == "-v" || arg == "--verbose")
        {
            ++opts.verbose;
        }
        else if (arg == "-h" || arg == "--help")
        {
            printHelp("kg");
            std::exit(0);
        }
        else
        {
            printUpdateUsageError("unknown option '" + arg + "'");
            return false;
        }
    }
    return true;
}

int runUpdateCmd(int argc, char** argv, int startIdx)
{
    kg::update::UpdateRunOptions opts;
    opts.kgVersion = KG_VERSION_STRING;

    if (!parseUpdateFlags(argc, argv, startIdx, opts))
    {
        return 1;
    }

    if (opts.dbPath.empty())
    {
        if (const char* env = std::getenv("KG_DB"))
        {
            opts.dbPath = env;
        }
    }
    if (opts.dbPath.empty())
    {
        printUpdateUsageError("missing required option -d/--db PATH");
        return 1;
    }

    return kg::update::runUpdate(opts);
}

// `kg dbdiff A B [--json] [--limit N] [--ignore CATEGORY]` -- a HIDDEN
// maintenance/equivalence-
// testing command (deliberately not listed in printHelp()) built on top of
// kg::update::{takeSnapshot,diffSnapshots}. Not part of `kg update` itself:
// this is the tool Phase 2/3 use to verify an incrementally-updated
// database is logically identical to a full `kg index` re-run. See
// docs/JSON_SCHEMA.md's `kg.dbdiff/1` section and
// docs/KNOWN_LIMITATIONS.md's incremental-indexing notes.
//
// Exit codes: 0 equal, 3 different, 4 a database couldn't be opened.
int runDbDiff(int argc, char** argv, int startIdx)
{
    std::vector<std::string> positional;
    bool wantJson = false;
    int limit = 50;
    kg::update::SnapshotOptions snapOptions;

    for (int i = startIdx; i < argc; ++i)
    {
        const std::string arg = argv[i];
        if (arg == "--json")
        {
            wantJson = true;
        }
        else if (arg == "--ignore")
        {
            if (i + 1 >= argc)
            {
                std::fprintf(stderr, "kg dbdiff: error: --ignore requires a value\n");
                return 4;
            }
            const std::string category = argv[++i];
            if (category == "shared-header-locations")
            {
                snapOptions.ignoreSharedHeaderLocations = true;
            }
            else
            {
                std::fprintf(
                    stderr,
                    "kg dbdiff: error: unknown --ignore category '%s' (known: shared-header-locations)\n",
                    category.c_str());
                return 4;
            }
        }
        else if (arg == "--limit")
        {
            if (i + 1 >= argc)
            {
                std::fprintf(stderr, "kg dbdiff: error: --limit requires a value\n");
                return 4;
            }
            limit = std::atoi(argv[++i]);
        }
        else if (!arg.empty() && arg[0] == '-')
        {
            std::fprintf(stderr, "kg dbdiff: error: unknown option '%s'\n", arg.c_str());
            return 4;
        }
        else
        {
            positional.push_back(arg);
        }
    }

    if (positional.size() != 2)
    {
        std::fprintf(stderr, "usage: kg dbdiff A B [--json] [--limit N] [--ignore shared-header-locations]\n");
        return 4;
    }

    std::unique_ptr<kg::update::Snapshot> a;
    std::unique_ptr<kg::update::Snapshot> b;
    try
    {
        a = std::make_unique<kg::update::Snapshot>(kg::update::takeSnapshot(positional[0], snapOptions));
        b = std::make_unique<kg::update::Snapshot>(kg::update::takeSnapshot(positional[1], snapOptions));
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "kg dbdiff: error: %s\n", e.what());
        return 4;
    }

    const kg::update::SnapshotDiff diff = kg::update::diffSnapshots(*a, *b);
    const bool equal = diff.onlyInA.empty() && diff.onlyInB.empty();

    if (wantJson)
    {
        nlohmann::json j;
        j["kg_version"] = KG_VERSION_STRING;
        j["schema"] = "kg.dbdiff/1";
        j["equal"] = equal;
        nlohmann::json counts;
        counts["only_in_a"] = diff.onlyInA.size();
        counts["only_in_b"] = diff.onlyInB.size();
        j["counts"] = std::move(counts);
        j["only_in_a"] = diff.onlyInA;
        j["only_in_b"] = diff.onlyInB;
        std::cout << j.dump() << "\n";
    }
    else
    {
        if (equal)
        {
            std::printf("kg dbdiff: %s and %s are logically equivalent\n", positional[0].c_str(), positional[1].c_str());
        }
        else
        {
            std::printf(
                "kg dbdiff: %s and %s differ (%zu only in A, %zu only in B)\n",
                positional[0].c_str(),
                positional[1].c_str(),
                diff.onlyInA.size(),
                diff.onlyInB.size());
            int shown = 0;
            for (const std::string& s : diff.onlyInA)
            {
                if (shown++ >= limit)
                {
                    break;
                }
                std::printf("  - only in A: %s\n", s.c_str());
            }
            shown = 0;
            for (const std::string& s : diff.onlyInB)
            {
                if (shown++ >= limit)
                {
                    break;
                }
                std::printf("  + only in B: %s\n", s.c_str());
            }
        }
    }

    return equal ? 0 : 3;
}

}  // namespace

int run(int argc, char** argv)
{
    const char* programName = (argc > 0) ? argv[0] : "kg";

    if (argc <= 1)
    {
        printHelp(programName);
        return 0;
    }

    const std::string first = argv[1];

    if (first == "index")
    {
        return runIndex(argc, argv, 2);
    }
    if (first == "query")
    {
        return runQuery(argc, argv, 2);
    }
    if (first == "update")
    {
        return runUpdateCmd(argc, argv, 2);
    }
    if (first == "dbdiff")
    {
        // Hidden maintenance command -- see runDbDiff()'s own comment.
        // Deliberately not in printHelp()'s usage text.
        return runDbDiff(argc, argv, 2);
    }

    for (int i = 1; i < argc; ++i)
    {
        const char* arg = argv[i];
        if (std::strcmp(arg, "--version") == 0)
        {
            printVersion();
            return 0;
        }
        if (std::strcmp(arg, "--help") == 0 || std::strcmp(arg, "-h") == 0)
        {
            printHelp(programName);
            return 0;
        }

        std::fprintf(stderr, "kg: unknown option '%s'\n", arg);
        printHelp(programName);
        return 1;
    }

    return 0;
}

}  // namespace kg::cli
