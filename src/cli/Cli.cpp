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

#include "kg/db/Database.h"
#include "kg/db/Enums.h"
#include "kg/db/NameDecode.h"
#include "kg/index/ArgSanitizer.h"
#include "kg/index/CompileDb.h"
#include "kg/index/CudaSupport.h"
#include "kg/index/FileRegistry.h"
#include "kg/index/IndexDriver.h"
#include "kg/out/JsonWriter.h"
#include "kg/out/TextWriter.h"
#include "kg/query/FuzzyMatcher.h"
#include "kg/query/QueryEngine.h"
#include "kg/query/SymbolResolver.h"

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
        "Exit codes: 0 ok, 1 usage error, 2 ambiguous symbol, 3 not found,\n"
        "            4 database missing/unreadable/wrong version.\n",
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
