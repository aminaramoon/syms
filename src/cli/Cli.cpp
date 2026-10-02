#include "kg/cli/Cli.h"

#include <algorithm>
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
#include <fstream>

#include "kg/common/Process.h"
#include "kg/db/Database.h"
#include "kg/db/Enums.h"
#include "kg/db/Meta.h"
#include "kg/db/NameDecode.h"
#include "kg/index/ArgSanitizer.h"
#include "kg/index/CompileDb.h"
#include "kg/index/CudaSupport.h"
#include "kg/index/FileRegistry.h"
#include "kg/index/IndexDriver.h"
#include "kg/index/Provenance.h"
#include "kg/common/Glob.h"
#include "kg/notes/BodyHash.h"
#include "kg/notes/Markdown.h"
#include "kg/notes/NoteModel.h"
#include "kg/notes/NotesStore.h"
#include "kg/notes/Scaffold.h"
#include "kg/notes/Sync.h"
#include "kg/out/JsonWriter.h"
#include "kg/out/TextWriter.h"
#include "kg/query/FuzzyMatcher.h"
#include "kg/query/QueryEngine.h"
#include "kg/query/SymbolResolver.h"
#include "kg/update/Git.h"
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
        "       %s notes lint --notes-dir DIR [OPTIONS]\n"
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
        "      --notes-dir DIR           record meta.kg_notes_dir (absolute) for `kg notes`/`kg update`\n"
        "                                 (default when omitted: <repo root>/notes, resolved lazily)\n"
        "      --remap OLD=NEW           literal substring path remap (repeatable)\n"
        "      --include GLOB            only process files matching GLOB (repeatable)\n"
        "      --exclude GLOB            never process files matching GLOB (repeatable)\n"
        "      --exclude-submodules      exclude every git submodule listed in the repo's\n"
        "                                 .gitmodules (adds <submodule>/** exclude globs)\n"
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
        "      --std STD                 fallback -std= value if none survives sanitizing,\n"
        "                                 for C++/CUDA TUs (default: c++17)\n"
        "      --c-std STD               fallback -std= value if none survives sanitizing,\n"
        "                                 for C TUs (default: gnu17)\n"
        "      --limit N                 only process the first N (post-filter) TUs\n"
        "      --no-vacuum               skip the VACUUM step after indexing\n"
        "      --json                    also print a JSON summary object to stdout\n"
        "  -j, --parallel N              extract N TUs concurrently (0 = all CPUs, default: 1 =\n"
        "                                 sequential). Output is byte-for-byte deterministic\n"
        "                                 regardless of N -- only wall-clock time changes. Peak\n"
        "                                 memory is roughly N x (largest TU's AST + batch)\n"
        "      --commit-every N          checkpoint (commit + flush kg_tu rows) every N consumed\n"
        "                                 TUs, so Ctrl-C/SIGTERM loses at most N TUs' work instead\n"
        "                                 of the whole run (default: 25; 0 = single transaction,\n"
        "                                 no checkpoints). Resume an interrupted run with `kg\n"
        "                                 update -d OUTPUT`.\n"
        "  -q, --quiet                   suppress the per-TU progress line, but keep the periodic\n"
        "                                 (every ~60s) running-summary line; repeat (-qq) to\n"
        "                                 silence that too (the final summary always prints)\n"
        "      --dry-run                 print each TU's fully-sanitized argv and exit,\n"
        "                                 instead of actually indexing\n"
        "  -v, --verbose                 increase verbosity (repeatable)\n"
        "\n"
        "query subcommand:\n"
        "  VERB                          one of: callers callees bases derived overrides\n"
        "                                 overridden-by members neighbors references\n"
        "                                 definition search file stats context\n"
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
        "      --no-notes                (context verb) omit the \"notes\" field entirely\n"
        "      --notes-full              (context verb, text mode) print the full note block, not just the brief\n"
        "      --json                    print machine-readable JSON instead of text\n"
        "                                 (see docs/JSON_SCHEMA.md)\n"
        "  -v, --verbose                 increase verbosity (repeatable)\n"
        "  -q, --quiet                   suppress the \"index incomplete (interrupted)\" notice\n"
        "                                 (printed when DB has kg_index_in_progress set; never\n"
        "                                 printed under --json)\n"
        "\n"
        "update subcommand (incrementally re-index files changed since the last index/update):\n"
        "  kg update -d DB                 everything else is inferred from the provenance stored in DB\n"
        "  -d, --db PATH                   existing .srctrldb produced by this kg version (required)\n"
        "      --repo DIR                  git repo (default: stored at index time)\n"
        "      --to REV                    treat REV as the new baseline (default: HEAD; worktree must match)\n"
        "  -c, --compile-commands PATH     (default: stored path)\n"
        "      --files PATH...             explicit changed files instead of git\n"
        "      --remap/--include/--exclude/--cuda-mode/--system-file-rows/--extra-arg/--drop-arg/--std/--c-std\n"
        "                                  override stored options (otherwise inherited)\n"
        "      --exclude-submodules        also exclude every git submodule in the repo's .gitmodules\n"
        "      --dry-run                   show what would be cleared/re-indexed, change nothing\n"
        "      --in-place                  modify DB directly instead of temp-copy + atomic rename\n"
        "  -j, --parallel N                extract N TUs concurrently for the re-index step (0 = all\n"
        "                                   CPUs, default: 1); deterministic regardless of N\n"
        "      --no-vacuum, --json, -q, -v\n"
        "\n"
        "notes subcommand (function-level knowledge attached to the symbol graph --\n"
        "see docs/NOTES_DESIGN.md):\n"
        "  lint     --notes-dir DIR [--strict] [--json]   parse a notes tree & report problems (no DB)\n"
        "  sync     -d DB [--notes-dir DIR] [--strict] [--json]   rebuild the kg_note* cache\n"
        "  show     -d DB SYMBOL [--section SEC] [--json]         print one symbol's note\n"
        "  status   -d DB [--stale] [--unfilled] [--unanchored] [--problems] [--json]\n"
        "  symbols  -d DB [--kind K]... [--file GLOB] [--json]    list indexed symbols\n"
        "  scaffold -d DB [--notes-dir DIR] (--all|--file GLOB|--class QNAME|SYMBOL)\n"
        "                 [--kind K]... [--dry-run] [--json]      write placeholder note blocks\n"
        "  accept   -d DB (SYMBOL|--all|--file GLOB) [--json]     re-anchor kg-hash/kg-commit\n"
        "  fmt      --notes-dir DIR [--check] [--json]            canonical re-render (no DB)\n"
        "\n"
        "Exit codes: 0 ok, 1 usage error, 2 ambiguous symbol / refused (legacy db / update in progress),\n"
        "            3 not found / lint problems found, 4 database missing/unreadable/wrong version,\n"
        "            5 nothing to do (update).\n",
        programName,
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
    int quietCount = 0;
    bool jsonSummary = false;
    bool noVacuum = false;
    kg::index::SystemFileRows systemFileRows = kg::index::SystemFileRows::Direct;
    int verbosity = 0;
    long limit = -1;  // -1 = no limit
    int parallel = 1;  // --parallel/-j; 0 = hardware_concurrency(), 1 (default) = sequential
    int commitEvery = 25;  // --commit-every; 0 = single transaction (no checkpoints)
    long stopAfter = -1;    // hidden --stop-after testing hook; -1 = disabled
    bool resourceDirGiven = false;
    bool cudaPathGiven = false;
    bool gccToolchainGiven = false;
    bool excludeSubmodules = false;
    std::string repoDir;
    std::string notesDirFlag;

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
        else if (arg == "--exclude-submodules")
        {
            excludeSubmodules = true;
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
        else if (arg == "--notes-dir")
        {
            notesDirFlag = requireValue(i, arg);
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
            ++quietCount;
        }
        else if (arg == "--parallel" || arg == "-j")
        {
            const std::string value = requireValue(i, arg);
            ++i;
            try
            {
                parallel = std::stoi(value);
            }
            catch (const std::exception&)
            {
                printIndexUsageError("invalid --parallel/-j value: " + value);
                return 2;
            }
            if (parallel < 0)
            {
                printIndexUsageError("--parallel/-j must be >= 0 (0 = hardware_concurrency())");
                return 2;
            }
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
        else if (arg == "--c-std")
        {
            sanitizerOptions.cStdFallback = requireValue(i, arg);
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
        else if (arg == "--commit-every")
        {
            const std::string value = requireValue(i, arg);
            ++i;
            try
            {
                commitEvery = std::stoi(value);
            }
            catch (const std::exception&)
            {
                printIndexUsageError("invalid --commit-every value: " + value);
                return 2;
            }
            if (commitEvery < 0)
            {
                printIndexUsageError("--commit-every must be >= 0 (0 = single transaction, no checkpoints)");
                return 2;
            }
        }
        else if (arg == "--stop-after")
        {
            // Hidden testing hook -- deliberately not in printHelp().
            const std::string value = requireValue(i, arg);
            ++i;
            try
            {
                stopAfter = std::stol(value);
            }
            catch (const std::exception&)
            {
                printIndexUsageError("invalid --stop-after value: " + value);
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

    if (excludeSubmodules)
    {
        const std::string startDir =
            !repoDir.empty() ? repoDir : std::filesystem::absolute(compileCommandsPath).parent_path().string();
        const std::optional<std::string> root = kg::update::topLevel(startDir);
        if (!root)
        {
            printIndexUsageError("--exclude-submodules: '" + startDir + "' is not inside a git repository (pass --repo DIR)");
            return 2;
        }
        for (const std::string& glob : kg::update::submoduleExcludeGlobs(*root))
        {
            if (std::find(dbOptions.excludeGlobs.begin(), dbOptions.excludeGlobs.end(), glob) == dbOptions.excludeGlobs.end())
            {
                dbOptions.excludeGlobs.push_back(glob);
                if (verbosity > 0)
                {
                    std::fprintf(stderr, "kg index: --exclude-submodules: excluding %s\n", glob.c_str());
                }
            }
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
    runOptions.quiet = quietCount >= 1;
    runOptions.superQuiet = quietCount >= 2;
    runOptions.jsonSummary = jsonSummary;
    runOptions.noVacuum = noVacuum;
    runOptions.verbose = verbosity > 0;
    runOptions.parallel = parallel;
    runOptions.commitEvery = commitEvery;
    runOptions.stopAfter = stopAfter;
    runOptions.dbOptions = dbOptions;
    runOptions.sanitizerOptions = sanitizerOptions;
    runOptions.systemFileRows = systemFileRows;
    runOptions.kgVersion = KG_VERSION_STRING;

    // Provenance is always built for `kg index` (Phase 1 groundwork for a
    // future `kg update`) -- an unresolvable git repo just leaves the repo
    // fields empty, it's never a hard error.
    runOptions.provenance = buildProvenance(compileCommandsPath, repoDir, runOptions, verbosity);
    if (!notesDirFlag.empty())
    {
        runOptions.provenance->notesDir = std::filesystem::absolute(notesDirFlag).lexically_normal().string();
    }

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
    bool quiet = false;
    bool noNotes = false;    // `kg query context`: omit the "notes" field entirely
    bool notesFull = false;  // `kg query context` text mode: print the full note block, not just brief
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
        else if (arg == "--no-notes")
        {
            flags.noNotes = true;
        }
        else if (arg == "--notes-full")
        {
            flags.notesFull = true;
        }
        else if (arg == "-v" || arg == "--verbose")
        {
            ++flags.verbosity;
        }
        else if (arg == "-q" || arg == "--quiet")
        {
            flags.quiet = true;
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

// Unions BFS results discovered by separate QueryEngine calls into one
// BfsResult, for `kg query context` (seed + callers + callees + one-hop
// overrides/overridden-by [+ one-hop bases/derived for CLASS/STRUCT seeds]).
// Nodes are deduped by id (first BfsNode seen for an id wins, since depth
// only matters for text-tree indentation and the seed -- always added
// first, at depth 0 -- should win any tie); edges are deduped by id.
kg::query::BfsResult mergeBfsResults(const std::vector<kg::query::BfsResult>& parts)
{
    kg::query::BfsResult out;
    std::unordered_set<int64_t> seenNodes;
    std::unordered_set<int64_t> seenEdges;
    for (const kg::query::BfsResult& part : parts)
    {
        for (const kg::query::BfsNode& n : part.nodes)
        {
            if (seenNodes.insert(n.id).second)
            {
                out.nodes.push_back(n);
            }
        }
        for (const kg::query::BfsEdge& e : part.edges)
        {
            if (seenEdges.insert(e.id).second)
            {
                out.edges.push_back(e);
            }
        }
        out.truncated = out.truncated || part.truncated;
    }
    return out;
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

// Forward declaration: defined further down alongside the other `kg notes`
// verbs, but `kg query context` (in runQuery(), below) also needs it.
void printCacheOutdatedHint(const std::vector<std::string>& overlayFiles);

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
        "context",
    };

    if (startIdx >= argc)
    {
        printQueryUsageError("missing VERB (expected one of: callers callees bases derived overrides overridden-by "
                              "members neighbors references definition search file stats context)");
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

    // Phase 8: a database left with kg_index_in_progress="1" is an
    // interrupted `kg index` run -- still fully queryable (read-only), just
    // incomplete. One-line stderr notice, suppressed by -q/--quiet or
    // --json (stdout stays pure JSON either way).
    if (!flags.json && !flags.quiet)
    {
        const std::optional<std::string> inProgress = kg::db::getMeta(dbPtr->handle(), "kg_index_in_progress");
        if (inProgress.has_value() && *inProgress == "1")
        {
            std::fprintf(stderr, "kg query: index incomplete (interrupted) -- run `kg update` to finish\n");
        }
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
        else if (verb == "context")
        {
            const auto t0 = std::chrono::steady_clock::now();

            std::vector<kg::query::BfsResult> parts;
            parts.push_back(engine.callers(seeds, flags.depth, flags.maxNodes, nodeKindMask));
            parts.push_back(engine.callees(seeds, flags.depth, flags.maxNodes, nodeKindMask));
            std::vector<int> edgeKinds = {kg::db::EdgeKind::CALL};

            parts.push_back(engine.overrides(seeds, nodeKindMask));
            parts.push_back(engine.overriddenBy(seeds, nodeKindMask));
            edgeKinds.push_back(kg::db::EdgeKind::OVERRIDE);

            // CLASS/STRUCT seeds also get one-hop bases/derived.
            bool anySeedIsClassLike = false;
            if (!seeds.empty())
            {
                db.runChunkedIn(
                    "SELECT type FROM node WHERE id IN ({IN})",
                    seeds,
                    [&](sqlite3_stmt* stmt) {
                        const int kind = sqlite3_column_int(stmt, 0);
                        if ((kind & (kg::db::NodeKind::CLASS | kg::db::NodeKind::STRUCT)) != 0)
                        {
                            anySeedIsClassLike = true;
                        }
                    });
            }
            if (anySeedIsClassLike)
            {
                parts.push_back(engine.bases(seeds, 1, 0, nodeKindMask));
                parts.push_back(engine.derived(seeds, 1, 0, nodeKindMask));
                edgeKinds.push_back(kg::db::EdgeKind::INHERITANCE);
            }

            const kg::query::BfsResult merged = mergeBfsResults(parts);
            const int64_t elapsedMs = millisSince(t0);

            std::unordered_map<std::string, nlohmann::json> notesJsonBySerializedName;
            std::unordered_map<int64_t, kg::out::NodeNoteText> notesTextByNodeId;
            std::vector<std::string> overlayFiles;
            if (!flags.noNotes)
            {
                const kg::notes::NoteLookup lookup = kg::notes::openNotes(db);
                overlayFiles = lookup.overlayFiles;

                // Build id -> serializedName for merged nodes, then look up
                // notes for those serialized names.
                for (const kg::query::BfsNode& n : merged.nodes)
                {
                    const auto it = lookup.bySerializedName.find(n.serializedName);
                    if (it == lookup.bySerializedName.end())
                    {
                        continue;
                    }
                    const kg::notes::StoredNote& note = it->second;

                    nlohmann::json noteJson;
                    noteJson["brief"] = note.brief;
                    noteJson["what"] = note.what;
                    noteJson["how"] = note.how;
                    noteJson["signature"] = note.signatureText;
                    noteJson["pre"] = note.pre;
                    noteJson["post"] = note.post;
                    noteJson["bugs"] = note.bugs;
                    noteJson["suggestions"] = note.suggestions;
                    noteJson["path"] = note.notesPath;
                    noteJson["line"] = note.headingLine;
                    noteJson["filled"] = note.filled;
                    noteJson["stale"] = note.stale;
                    noteJson["note_hash"] = note.noteHash;
                    noteJson["current_body_hash"] = note.currentBodyHash;
                    noteJson["commit"] = note.noteCommit;
                    notesJsonBySerializedName[n.serializedName] = noteJson;

                    kg::out::NodeNoteText text;
                    text.brief = note.brief;
                    text.filled = note.filled;
                    text.stale = note.stale;
                    if (flags.notesFull)
                    {
                        kg::notes::NoteBlock block;
                        block.heading = note.qualifiedName;
                        block.bodyHash = note.noteHash;
                        block.commit = note.noteCommit;
                        block.brief = note.brief;
                        block.what = {true, 0, note.what};
                        block.how = {true, 0, note.how};
                        block.signature = {true, 0, note.signatureText};
                        block.pre = {true, 0, note.pre};
                        block.post = {true, 0, note.post};
                        block.bugs = {true, 0, note.bugs};
                        block.suggestions = {true, 0, note.suggestions};
                        text.fullMarkdown = kg::notes::renderBlock(block);
                    }
                    notesTextByNodeId[n.id] = std::move(text);
                }

                if (!flags.json)
                {
                    printCacheOutdatedHint(overlayFiles);
                }
            }

            if (flags.json)
            {
                jsonCtx.depth = flags.depth;
                jsonCtx.maxNodes = flags.maxNodes;
                jsonCtx.edgeKinds = edgeKinds;
                const std::unordered_map<std::string, nlohmann::json>* notesPtr = flags.noNotes ? nullptr : &notesJsonBySerializedName;
                std::cout << jsonWriter->graph(jsonCtx, merged, elapsedMs, notesPtr).dump() << "\n";
            }
            else
            {
                std::vector<int64_t> ids;
                ids.reserve(merged.nodes.size());
                for (const kg::query::BfsNode& n : merged.nodes)
                {
                    ids.push_back(n.id);
                }
                const auto locs = engine.primaryLocations(ids);
                const std::unordered_map<int64_t, kg::out::NodeNoteText>* notesPtr = flags.noNotes ? nullptr : &notesTextByNodeId;
                kg::out::writeBfsResult(std::cout, merged, locs, flags.verbosity > 0, notesPtr, flags.notesFull);
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
        else if (arg == "--c-std")
        {
            opts.cStdOverride = requireValue(i, arg);
            ++i;
            opts.cStdGiven = true;
        }
        else if (arg == "--dry-run")
        {
            opts.dryRun = true;
        }
        else if (arg == "--exclude-submodules")
        {
            opts.excludeSubmodules = true;
        }
        else if (arg == "--in-place")
        {
            opts.inPlace = true;
        }
        else if (arg == "--parallel" || arg == "-j")
        {
            const std::string value = requireValue(i, arg);
            ++i;
            try
            {
                opts.parallel = std::stoi(value);
            }
            catch (const std::exception&)
            {
                printUpdateUsageError("invalid --parallel/-j value: " + value);
                return false;
            }
            if (opts.parallel < 0)
            {
                printUpdateUsageError("--parallel/-j must be >= 0 (0 = hardware_concurrency())");
                return false;
            }
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

// `kg notes lint --notes-dir DIR [--strict] [--json]` -- Phase 1 of
// docs/NOTES_DESIGN.md. Walks DIR recursively for `*.md`, parses each with
// kg::notes::parseNotesFile (no DB involved), and reports every Problem.
// Exit codes: 0 no problems, 1 usage error, 3 problems found.
int runNotesLint(int argc, char** argv, int startIdx)
{
    std::string notesDir;
    bool strict = false;
    bool wantJson = false;

    for (int i = startIdx; i < argc; ++i)
    {
        const std::string arg = argv[i];
        if (arg == "--notes-dir")
        {
            if (i + 1 >= argc)
            {
                std::fprintf(stderr, "kg notes lint: --notes-dir requires a value\n");
                return 1;
            }
            notesDir = argv[++i];
        }
        else if (arg == "--strict")
        {
            strict = true;
        }
        else if (arg == "--json")
        {
            wantJson = true;
        }
        else if (arg == "-h" || arg == "--help")
        {
            std::printf("Usage: kg notes lint --notes-dir DIR [--strict] [--json]\n");
            return 0;
        }
        else
        {
            std::fprintf(stderr, "kg notes lint: unknown option '%s'\n", arg.c_str());
            return 1;
        }
    }

    if (notesDir.empty())
    {
        std::fprintf(stderr, "kg notes lint: missing required option --notes-dir DIR\n");
        return 1;
    }
    if (!std::filesystem::is_directory(notesDir))
    {
        std::fprintf(stderr, "kg notes lint: '%s' is not a directory\n", notesDir.c_str());
        return 1;
    }

    struct Finding
    {
        std::string path;
        int line;
        std::string kind;
        std::string message;
    };

    std::vector<std::string> mdFiles;
    for (const std::filesystem::directory_entry& e : std::filesystem::recursive_directory_iterator(notesDir))
    {
        if (e.is_regular_file() && e.path().extension() == ".md")
        {
            mdFiles.push_back(e.path().string());
        }
    }
    std::sort(mdFiles.begin(), mdFiles.end());

    std::size_t totalBlocks = 0;
    std::size_t totalFilled = 0;
    std::vector<Finding> findings;

    for (const std::string& fullPath : mdFiles)
    {
        std::ifstream in(fullPath, std::ios::binary);
        std::string content((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());

        const std::string relPath = std::filesystem::relative(fullPath, notesDir).generic_string();
        const kg::notes::NotesFile parsed = kg::notes::parseNotesFile(content, relPath, strict);

        totalBlocks += parsed.blocks.size();
        for (const kg::notes::NoteBlock& b : parsed.blocks)
        {
            if (b.filled())
            {
                ++totalFilled;
            }
        }
        for (const kg::notes::Problem& p : parsed.problems)
        {
            findings.push_back(Finding{relPath, p.line, p.kind, p.message});
        }
    }

    std::stable_sort(findings.begin(), findings.end(), [](const Finding& a, const Finding& b) {
        if (a.path != b.path) return a.path < b.path;
        return a.line < b.line;
    });

    if (wantJson)
    {
        nlohmann::json j;
        j["kg_version"] = KG_VERSION_STRING;
        j["schema"] = "kg.notes-lint/1";
        j["notes_dir"] = notesDir;
        j["files"] = mdFiles.size();
        j["blocks"] = totalBlocks;
        j["filled"] = totalFilled;
        nlohmann::json problems = nlohmann::json::array();
        for (const Finding& f : findings)
        {
            nlohmann::json p;
            p["path"] = f.path;
            p["line"] = f.line;
            p["kind"] = f.kind;
            p["message"] = f.message;
            problems.push_back(std::move(p));
        }
        j["problems"] = std::move(problems);
        std::cout << j.dump() << "\n";
    }
    else
    {
        for (const Finding& f : findings)
        {
            std::printf("%s:%d: %s: %s\n", f.path.c_str(), f.line, f.kind.c_str(), f.message.c_str());
        }
        std::printf("%zu files, %zu blocks (%zu filled), %zu problems\n", mdFiles.size(), totalBlocks, totalFilled, findings.size());
    }

    return findings.empty() ? 0 : 3;
}

// Resolves the notes directory a DB-backed notes verb should use, absent an
// explicit --notes-dir: `meta.kg_notes_dir` (set by a previous `kg notes
// sync`), else `<meta.kg_repo_root>/notes`. Returns "" if neither is
// available (caller reports a usage error).
std::string resolveNotesDirDefault(const std::string& dbPath)
{
    try
    {
        kg::db::Database db(dbPath);
        const std::optional<std::string> stored = kg::db::getMeta(db.handle(), "kg_notes_dir");
        if (stored.has_value() && !stored->empty())
        {
            return *stored;
        }
        const std::optional<std::string> repoRoot = kg::db::getMeta(db.handle(), "kg_repo_root");
        if (repoRoot.has_value() && !repoRoot->empty())
        {
            return (std::filesystem::path(*repoRoot) / "notes").string();
        }
    }
    catch (const std::exception&)
    {
        // Falls through to "" -- the caller's own Database(dbPath) open
        // (right after) reports the real error message.
    }
    return "";
}

// `kg notes sync -d DB [--notes-dir DIR] [--strict] [--json]` -- Phase 2.
// Rebuilds the `kg_note*` cache from the markdown tree; see
// kg::notes::syncNotes()'s own doc comment for the exact algorithm.
int runNotesSync(int argc, char** argv, int startIdx)
{
    std::string dbPath;
    std::string notesDirFlag;
    bool strict = false;
    bool wantJson = false;

    for (int i = startIdx; i < argc; ++i)
    {
        const std::string arg = argv[i];
        if (arg == "-d" || arg == "--db")
        {
            if (i + 1 >= argc) { std::fprintf(stderr, "kg notes sync: '%s' requires a value\n", arg.c_str()); return 1; }
            dbPath = argv[++i];
        }
        else if (arg == "--notes-dir")
        {
            if (i + 1 >= argc) { std::fprintf(stderr, "kg notes sync: --notes-dir requires a value\n"); return 1; }
            notesDirFlag = argv[++i];
        }
        else if (arg == "--strict") { strict = true; }
        else if (arg == "--json") { wantJson = true; }
        else if (arg == "-h" || arg == "--help")
        {
            std::printf("Usage: kg notes sync -d DB [--notes-dir DIR] [--strict] [--json]\n");
            return 0;
        }
        else
        {
            std::fprintf(stderr, "kg notes sync: unknown option '%s'\n", arg.c_str());
            return 1;
        }
    }

    if (dbPath.empty())
    {
        if (const char* env = std::getenv("KG_DB")) { dbPath = env; }
    }
    if (dbPath.empty())
    {
        std::fprintf(stderr, "kg notes sync: missing required option -d/--db PATH\n");
        return 1;
    }

    const std::string notesDir = !notesDirFlag.empty() ? notesDirFlag : resolveNotesDirDefault(dbPath);
    if (notesDir.empty())
    {
        std::fprintf(
            stderr,
            "kg notes sync: no notes dir given and none stored (nor a repo root) in '%s' -- pass --notes-dir DIR\n",
            dbPath.c_str());
        return 1;
    }

    kg::notes::SyncResult result;
    try
    {
        result = kg::notes::syncNotes(dbPath, notesDir, strict);
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "kg notes sync: %s\n", e.what());
        return 4;
    }

    const std::string absNotesDir = std::filesystem::absolute(notesDir).string();

    if (wantJson)
    {
        nlohmann::json j;
        j["kg_version"] = KG_VERSION_STRING;
        j["schema"] = "kg.notes-sync/1";
        j["database"] = dbPath;
        j["notes_dir"] = absNotesDir;
        nlohmann::json counts;
        counts["files"] = result.counts.files;
        counts["blocks"] = result.counts.blocks;
        counts["matched"] = result.counts.matched;
        counts["unmatched"] = result.counts.unmatched;
        counts["ambiguous"] = result.counts.ambiguous;
        counts["duplicate_target"] = result.counts.duplicateTarget;
        counts["filled"] = result.counts.filled;
        counts["stale"] = result.counts.stale;
        counts["unanchored"] = result.counts.unanchored;
        counts["lint_problems"] = result.counts.lintProblems;
        counts["signature_mismatch"] = result.counts.signatureMismatch;
        counts["orphaned_files"] = result.counts.orphanedFiles;
        j["counts"] = std::move(counts);
        nlohmann::json problems = nlohmann::json::array();
        for (const kg::notes::SyncProblem& p : result.problems)
        {
            nlohmann::json pj;
            pj["path"] = p.path;
            pj["line"] = p.line;
            pj["heading"] = p.heading;
            pj["kind"] = p.kind;
            pj["message"] = p.message;
            problems.push_back(std::move(pj));
        }
        j["problems"] = std::move(problems);
        std::cout << j.dump() << "\n";
    }
    else
    {
        for (const kg::notes::SyncProblem& p : result.problems)
        {
            std::printf("%s:%d: %s: %s\n", p.path.c_str(), p.line, p.kind.c_str(), p.message.c_str());
        }
        std::printf(
            "%zu files, %zu blocks: %zu matched (%zu filled, %zu stale, %zu unanchored, %zu signature-mismatch), "
            "%zu unmatched, %zu ambiguous, %zu duplicate-target, %zu orphaned-file, %zu lint problems\n",
            result.counts.files,
            result.counts.blocks,
            result.counts.matched,
            result.counts.filled,
            result.counts.stale,
            result.counts.unanchored,
            result.counts.signatureMismatch,
            result.counts.unmatched,
            result.counts.ambiguous,
            result.counts.duplicateTarget,
            result.counts.orphanedFiles,
            result.counts.lintProblems);
    }

    return (strict && !result.problems.empty()) ? 3 : 0;
}

void printCacheOutdatedHint(const std::vector<std::string>& overlayFiles)
{
    if (!overlayFiles.empty())
    {
        std::fprintf(
            stderr,
            "kg notes: notes cache out of date (%zu file%s changed) -- run `kg notes sync`\n",
            overlayFiles.size(),
            overlayFiles.size() == 1 ? "" : "s");
    }
}

// `kg notes show -d DB SYMBOL [--section SEC] [--json]` -- Phase 2. Resolves
// SYMBOL exactly like `kg query`, then looks the note up via the overlay-
// aware kg::notes::openNotes(). Exit codes: 0 ok, 1 usage, 2 ambiguous,
// 3 not found / no note, 4 db error.
int runNotesShow(int argc, char** argv, int startIdx)
{
    std::string dbPath;
    std::string section;
    bool wantJson = false;
    std::string spec;
    bool haveSpec = false;

    for (int i = startIdx; i < argc; ++i)
    {
        const std::string arg = argv[i];
        if (arg == "-d" || arg == "--db")
        {
            if (i + 1 >= argc) { std::fprintf(stderr, "kg notes show: '%s' requires a value\n", arg.c_str()); return 1; }
            dbPath = argv[++i];
        }
        else if (arg == "--section")
        {
            if (i + 1 >= argc) { std::fprintf(stderr, "kg notes show: --section requires a value\n"); return 1; }
            section = argv[++i];
        }
        else if (arg == "--json") { wantJson = true; }
        else if (arg == "-h" || arg == "--help")
        {
            std::printf("Usage: kg notes show -d DB SYMBOL [--section brief|what|how|signature|pre|post|bugs|suggestions] [--json]\n");
            return 0;
        }
        else if (!arg.empty() && arg[0] == '-' && arg != "-")
        {
            std::fprintf(stderr, "kg notes show: unknown option '%s'\n", arg.c_str());
            return 1;
        }
        else
        {
            if (haveSpec) { std::fprintf(stderr, "kg notes show: unexpected extra argument '%s'\n", arg.c_str()); return 1; }
            spec = arg;
            haveSpec = true;
        }
    }

    if (dbPath.empty())
    {
        if (const char* env = std::getenv("KG_DB")) { dbPath = env; }
        else { dbPath = "./kg.srctrldb"; }
    }
    if (!haveSpec)
    {
        std::fprintf(stderr, "kg notes show: missing required SYMBOL argument\n");
        return 1;
    }
    static const std::unordered_set<std::string> kSections = {"brief", "what", "how", "signature", "pre", "post", "bugs", "suggestions"};
    if (!section.empty() && kSections.find(section) == kSections.end())
    {
        std::fprintf(stderr, "kg notes show: unknown --section '%s'\n", section.c_str());
        return 1;
    }

    std::unique_ptr<kg::db::Database> dbPtr;
    try
    {
        dbPtr = std::make_unique<kg::db::Database>(dbPath);
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "kg notes show: %s\n", e.what());
        return 4;
    }
    kg::db::Database& db = *dbPtr;
    kg::query::QueryEngine engine(db);

    std::unique_ptr<kg::out::json::JsonWriter> jsonWriter;
    if (wantJson)
    {
        jsonWriter = std::make_unique<kg::out::json::JsonWriter>(db, engine, KG_VERSION_STRING);
    }

    kg::out::json::QueryContext jsonCtx;
    jsonCtx.verb = "show";
    jsonCtx.db = dbPath;
    jsonCtx.symbolOrTextOrPath = spec;

    const kg::query::ResolveResult rr = kg::query::resolveSymbol(db, spec, nullptr, nullptr);
    if (rr.outcome == kg::query::ResolveOutcome::Ambiguous)
    {
        if (wantJson) { std::cout << jsonWriter->ambiguous(jsonCtx, rr.candidates).dump() << "\n"; }
        else
        {
            std::printf("kg notes show: '%s' is ambiguous (%zu candidates):\n", spec.c_str(), rr.candidates.size());
            kg::out::writeCandidateTable(std::cout, toCandidateRows(engine, rr.candidates));
        }
        return 2;
    }
    if (rr.outcome == kg::query::ResolveOutcome::NotFound)
    {
        const std::vector<kg::query::SearchHit> hits = engine.search(spec, nullptr, 5);
        if (wantJson) { std::cout << jsonWriter->notFound(jsonCtx, hits).dump() << "\n"; }
        else { std::fprintf(stderr, "kg notes show: symbol '%s' not found.\n", spec.c_str()); }
        return 3;
    }

    const kg::query::ResolvedSymbol& sym = rr.candidates.front();
    jsonCtx.symbolIds = {sym.id};

    const kg::notes::NoteLookup lookup = kg::notes::openNotes(db);
    printCacheOutdatedHint(lookup.overlayFiles);

    const auto it = lookup.bySerializedName.find(sym.serializedName);
    const bool hasNote = it != lookup.bySerializedName.end();

    if (wantJson)
    {
        const kg::query::DefinitionResult defResult = engine.definition({sym.id});
        nlohmann::json defJson = jsonWriter->definition(jsonCtx, defResult, 0);

        nlohmann::json j;
        j["kg_version"] = KG_VERSION_STRING;
        j["schema"] = "kg.note/1";
        nlohmann::json q;
        q["verb"] = "show";
        q["db"] = dbPath;
        q["symbol"] = spec;
        q["symbol_ids"] = jsonCtx.symbolIds;
        j["query"] = std::move(q);
        j["symbol"] = defJson["symbol"];

        if (hasNote)
        {
            const kg::notes::StoredNote& n = it->second;
            nlohmann::json note;
            note["brief"] = n.brief;
            note["what"] = n.what;
            note["how"] = n.how;
            note["signature"] = n.signatureText;
            note["pre"] = n.pre;
            note["post"] = n.post;
            note["bugs"] = n.bugs;
            note["suggestions"] = n.suggestions;
            note["path"] = n.notesPath;
            note["line"] = n.headingLine;
            note["filled"] = n.filled;
            note["stale"] = n.stale;
            note["note_hash"] = n.noteHash;
            note["current_body_hash"] = n.currentBodyHash;
            note["commit"] = n.noteCommit;
            j["note"] = std::move(note);
        }
        else
        {
            j["note"] = nullptr;
        }
        j["cache_outdated_files"] = lookup.overlayFiles;
        std::cout << j.dump() << "\n";
        return hasNote ? 0 : 3;
    }

    if (!hasNote)
    {
        std::fprintf(stderr, "kg notes show: symbol '%s' has no note.\n", spec.c_str());
        return 3;
    }

    const kg::notes::StoredNote& n = it->second;
    if (!section.empty())
    {
        if (section == "brief") { std::printf("%s\n", n.brief.c_str()); }
        else if (section == "what") { std::printf("%s\n", n.what.c_str()); }
        else if (section == "how") { std::printf("%s\n", n.how.c_str()); }
        else if (section == "signature") { std::printf("%s\n", n.signatureText.c_str()); }
        else
        {
            const std::vector<std::string>* items = section == "pre" ? &n.pre : section == "post" ? &n.post : section == "bugs" ? &n.bugs : &n.suggestions;
            for (std::size_t i = 0; i < items->size(); ++i)
            {
                std::printf("%zu. %s\n", i + 1, (*items)[i].c_str());
            }
        }
        return 0;
    }

    kg::notes::NoteBlock block;
    block.heading = n.qualifiedName;
    block.bodyHash = n.noteHash;
    block.commit = n.noteCommit;
    block.brief = n.brief;
    block.what = {true, 0, n.what};
    block.how = {true, 0, n.how};
    block.signature = {true, 0, n.signatureText};
    block.pre = {true, 0, n.pre};
    block.post = {true, 0, n.post};
    block.bugs = {true, 0, n.bugs};
    block.suggestions = {true, 0, n.suggestions};
    std::cout << kg::notes::renderBlock(block) << "\n";
    return 0;
}

// `kg notes status -d DB [--stale] [--unfilled] [--unanchored] [--problems]
// [--json]` -- Phase 2 coverage report over FUNCTION+METHOD nodes.
int runNotesStatus(int argc, char** argv, int startIdx)
{
    std::string dbPath;
    bool wantStale = false, wantUnfilled = false, wantUnanchored = false, wantProblems = false, wantJson = false;

    for (int i = startIdx; i < argc; ++i)
    {
        const std::string arg = argv[i];
        if (arg == "-d" || arg == "--db")
        {
            if (i + 1 >= argc) { std::fprintf(stderr, "kg notes status: '%s' requires a value\n", arg.c_str()); return 1; }
            dbPath = argv[++i];
        }
        else if (arg == "--stale") { wantStale = true; }
        else if (arg == "--unfilled") { wantUnfilled = true; }
        else if (arg == "--unanchored") { wantUnanchored = true; }
        else if (arg == "--problems") { wantProblems = true; }
        else if (arg == "--json") { wantJson = true; }
        else if (arg == "-h" || arg == "--help")
        {
            std::printf("Usage: kg notes status -d DB [--stale] [--unfilled] [--unanchored] [--problems] [--json]\n");
            return 0;
        }
        else
        {
            std::fprintf(stderr, "kg notes status: unknown option '%s'\n", arg.c_str());
            return 1;
        }
    }
    if (dbPath.empty())
    {
        if (const char* env = std::getenv("KG_DB")) { dbPath = env; }
        else { dbPath = "./kg.srctrldb"; }
    }

    std::unique_ptr<kg::db::Database> dbPtr;
    try
    {
        dbPtr = std::make_unique<kg::db::Database>(dbPath);
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "kg notes status: %s\n", e.what());
        return 4;
    }
    kg::db::Database& db = *dbPtr;

    // "coverage universe": every FUNCTION|METHOD node with a `symbol` row.
    std::unordered_map<std::string, int64_t> coverageNodeBySerializedName;
    db.run(
        "SELECT n.id, n.serialized_name FROM node n JOIN symbol s ON s.id = n.id WHERE (n.type & ?) != 0",
        [&](sqlite3_stmt* stmt) { sqlite3_bind_int(stmt, 1, kg::db::NodeKind::FUNCTION | kg::db::NodeKind::METHOD); },
        [&](sqlite3_stmt* stmt) {
            const int64_t id = sqlite3_column_int64(stmt, 0);
            const unsigned char* text = sqlite3_column_text(stmt, 1);
            coverageNodeBySerializedName[text != nullptr ? reinterpret_cast<const char*>(text) : std::string()] = id;
        });

    const kg::notes::NoteLookup lookup = kg::notes::openNotes(db);
    printCacheOutdatedHint(lookup.overlayFiles);

    std::size_t noted = 0, filled = 0, stale = 0, unanchored = 0, signatureMismatch = 0;
    std::vector<std::string> staleList, unfilledList, unanchoredList;
    for (const auto& [serializedName, note] : lookup.bySerializedName)
    {
        if (coverageNodeBySerializedName.find(serializedName) == coverageNodeBySerializedName.end())
        {
            continue;  // not a FUNCTION/METHOD -- outside the coverage universe
        }
        ++noted;
        const std::string entry = note.qualifiedName + " (" + note.notesPath + ":" + std::to_string(note.headingLine) + ")";
        if (note.filled) { ++filled; } else { unfilledList.push_back(entry); }
        if (note.stale) { ++stale; staleList.push_back(entry); }
        if (note.noteHash.empty()) { ++unanchored; unanchoredList.push_back(entry); }
    }
    std::sort(staleList.begin(), staleList.end());
    std::sort(unfilledList.begin(), unfilledList.end());
    std::sort(unanchoredList.begin(), unanchoredList.end());

    std::vector<kg::notes::SyncProblem> problemsList;
    if (kg::db::getMeta(db.handle(), "kg_notes_dir").has_value())
    {
        try
        {
            db.run(
                "SELECT notes_path, line, heading, kind, message FROM kg_note_problem ORDER BY notes_path, line",
                [&](sqlite3_stmt* stmt) {
                    auto text = [&](int col) {
                        const unsigned char* t = sqlite3_column_text(stmt, col);
                        return t != nullptr ? std::string(reinterpret_cast<const char*>(t)) : std::string();
                    };
                    problemsList.push_back(kg::notes::SyncProblem{text(0), sqlite3_column_int(stmt, 1), text(2), text(3), text(4)});
                });
        }
        catch (const std::exception&)
        {
            // kg_note_problem doesn't exist yet (sync never ran) -- leave empty.
        }
    }
    for (const kg::notes::SyncProblem& p : problemsList)
    {
        if (p.kind == "signature-mismatch")
        {
            ++signatureMismatch;
        }
    }

    if (wantJson)
    {
        nlohmann::json j;
        j["kg_version"] = KG_VERSION_STRING;
        j["schema"] = "kg.notes-status/1";
        j["database"] = dbPath;
        nlohmann::json counts;
        counts["total"] = coverageNodeBySerializedName.size();
        counts["noted"] = noted;
        counts["filled"] = filled;
        counts["stale"] = stale;
        counts["unanchored"] = unanchored;
        counts["signature_mismatch"] = signatureMismatch;
        counts["problems"] = problemsList.size();
        j["counts"] = std::move(counts);
        if (wantStale) { j["stale"] = staleList; }
        if (wantUnfilled) { j["unfilled"] = unfilledList; }
        if (wantUnanchored) { j["unanchored"] = unanchoredList; }
        if (wantProblems)
        {
            nlohmann::json pj = nlohmann::json::array();
            for (const kg::notes::SyncProblem& p : problemsList)
            {
                nlohmann::json e;
                e["path"] = p.path;
                e["line"] = p.line;
                e["heading"] = p.heading;
                e["kind"] = p.kind;
                e["message"] = p.message;
                pj.push_back(std::move(e));
            }
            j["problems"] = std::move(pj);
        }
        j["cache_outdated_files"] = lookup.overlayFiles;
        std::cout << j.dump() << "\n";
        return 0;
    }

    std::printf(
        "%zu indexed FUNCTION/METHOD symbols, %zu noted (%zu filled, %zu stale, %zu unanchored, %zu "
        "signature-mismatch), %zu problems\n",
        coverageNodeBySerializedName.size(),
        noted,
        filled,
        stale,
        unanchored,
        signatureMismatch,
        problemsList.size());
    if (wantStale) { std::printf("-- stale --\n"); for (const std::string& s : staleList) std::printf("%s\n", s.c_str()); }
    if (wantUnfilled) { std::printf("-- unfilled --\n"); for (const std::string& s : unfilledList) std::printf("%s\n", s.c_str()); }
    if (wantUnanchored) { std::printf("-- unanchored --\n"); for (const std::string& s : unanchoredList) std::printf("%s\n", s.c_str()); }
    if (wantProblems)
    {
        std::printf("-- problems --\n");
        for (const kg::notes::SyncProblem& p : problemsList)
        {
            std::printf("%s:%d: %s: %s\n", p.path.c_str(), p.line, p.kind.c_str(), p.message.c_str());
        }
    }
    return 0;
}

// `kg notes symbols -d DB [--kind K]... [--file GLOB]... [--json]` --
// Phase 2. Lists indexed symbols in the exact shape scripts/kg-notes-
// fake.py --symbols-json consumes; Phase 3's `scaffold` shares this code
// path.
int runNotesSymbols(int argc, char** argv, int startIdx)
{
    std::string dbPath;
    std::vector<std::string> kindNames;
    std::vector<std::string> fileGlobs;
    bool wantJson = false;

    for (int i = startIdx; i < argc; ++i)
    {
        const std::string arg = argv[i];
        if (arg == "-d" || arg == "--db")
        {
            if (i + 1 >= argc) { std::fprintf(stderr, "kg notes symbols: '%s' requires a value\n", arg.c_str()); return 1; }
            dbPath = argv[++i];
        }
        else if (arg == "--kind")
        {
            if (i + 1 >= argc) { std::fprintf(stderr, "kg notes symbols: --kind requires a value\n"); return 1; }
            kindNames.push_back(argv[++i]);
        }
        else if (arg == "--file")
        {
            if (i + 1 >= argc) { std::fprintf(stderr, "kg notes symbols: --file requires a value\n"); return 1; }
            fileGlobs.push_back(argv[++i]);
        }
        else if (arg == "--json") { wantJson = true; }
        else if (arg == "-h" || arg == "--help")
        {
            std::printf("Usage: kg notes symbols -d DB [--kind FUNCTION|METHOD|CLASS|STRUCT]... [--file GLOB] [--json]\n");
            return 0;
        }
        else
        {
            std::fprintf(stderr, "kg notes symbols: unknown option '%s'\n", arg.c_str());
            return 1;
        }
    }
    if (dbPath.empty())
    {
        if (const char* env = std::getenv("KG_DB")) { dbPath = env; }
        else { dbPath = "./kg.srctrldb"; }
    }

    int mask = 0;
    for (const std::string& name : kindNames)
    {
        const std::optional<int> k = kg::db::nodeKindFromString(name);
        if (!k.has_value())
        {
            std::fprintf(stderr, "kg notes symbols: unknown --kind value '%s'\n", name.c_str());
            return 1;
        }
        mask |= *k;
    }
    if (mask == 0)
    {
        mask = kg::db::NodeKind::FUNCTION | kg::db::NodeKind::METHOD;
    }

    std::unique_ptr<kg::db::Database> dbPtr;
    try
    {
        dbPtr = std::make_unique<kg::db::Database>(dbPath);
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "kg notes symbols: %s\n", e.what());
        return 4;
    }
    kg::db::Database& db = *dbPtr;

    const std::string commit = kg::notes::currentCommit(db);
    const std::vector<kg::notes::SymbolRow> rows = kg::notes::listSymbols(db, mask, fileGlobs);

    if (wantJson)
    {
        nlohmann::json j;
        j["kg_version"] = KG_VERSION_STRING;
        j["schema"] = "kg.notes-symbols/1";
        j["database"] = dbPath;
        nlohmann::json symbols = nlohmann::json::array();
        for (const kg::notes::SymbolRow& r : rows)
        {
            nlohmann::json s;
            s["source_path"] = r.sourcePath;
            s["qualified_name"] = r.qualifiedName;
            s["signature"] = r.signature;
            s["body_hash"] = r.bodyHash;
            s["commit"] = commit;
            s["node_id"] = r.nodeId;
            symbols.push_back(std::move(s));
        }
        j["symbols"] = std::move(symbols);
        std::cout << j.dump() << "\n";
    }
    else
    {
        for (const kg::notes::SymbolRow& r : rows)
        {
            std::printf("%s\t%s\t%s\t#%lld\n", r.sourcePath.c_str(), r.qualifiedName.c_str(), r.signature.c_str(), static_cast<long long>(r.nodeId));
        }
        std::printf("%zu symbols\n", rows.size());
    }
    return 0;
}

// `kg notes scaffold -d DB [--notes-dir DIR] (--all | --file GLOB | --class
// QNAME | SYMBOL) [--kind K]... [--dry-run] [--json]` -- Phase 3. Symbol
// selection funnels into kg::notes::SymbolRow via listSymbols() (--all/
// --file), QueryEngine::members() (--class, resolved like `kg query`'s own
// --class-shaped specs), or resolveSymbol() (SYMBOL); the actual markdown
// writing is kg::notes::scaffoldNotes(). Exit codes: 0 ok, 1 usage, 2
// ambiguous SYMBOL/--class, 3 not found / no definition location, 4 db error.
int runNotesScaffold(int argc, char** argv, int startIdx)
{
    std::string dbPath;
    std::string notesDirFlag;
    bool all = false;
    std::string fileGlob;
    std::string classQName;
    std::string symbolSpec;
    bool haveSymbol = false;
    std::vector<std::string> kindNames;
    bool dryRun = false;
    bool wantJson = false;

    for (int i = startIdx; i < argc; ++i)
    {
        const std::string arg = argv[i];
        if (arg == "-d" || arg == "--db")
        {
            if (i + 1 >= argc) { std::fprintf(stderr, "kg notes scaffold: '%s' requires a value\n", arg.c_str()); return 1; }
            dbPath = argv[++i];
        }
        else if (arg == "--notes-dir")
        {
            if (i + 1 >= argc) { std::fprintf(stderr, "kg notes scaffold: --notes-dir requires a value\n"); return 1; }
            notesDirFlag = argv[++i];
        }
        else if (arg == "--all") { all = true; }
        else if (arg == "--file")
        {
            if (i + 1 >= argc) { std::fprintf(stderr, "kg notes scaffold: --file requires a value\n"); return 1; }
            fileGlob = argv[++i];
        }
        else if (arg == "--class")
        {
            if (i + 1 >= argc) { std::fprintf(stderr, "kg notes scaffold: --class requires a value\n"); return 1; }
            classQName = argv[++i];
        }
        else if (arg == "--kind")
        {
            if (i + 1 >= argc) { std::fprintf(stderr, "kg notes scaffold: --kind requires a value\n"); return 1; }
            kindNames.push_back(argv[++i]);
        }
        else if (arg == "--dry-run") { dryRun = true; }
        else if (arg == "--json") { wantJson = true; }
        else if (arg == "-h" || arg == "--help")
        {
            std::printf(
                "Usage: kg notes scaffold -d DB [--notes-dir DIR] (--all | --file SRC-GLOB | --class QNAME | SYMBOL)\n"
                "                          [--kind FUNCTION|METHOD]... [--dry-run] [--json]\n");
            return 0;
        }
        else if (!arg.empty() && arg[0] == '-' && arg != "-")
        {
            std::fprintf(stderr, "kg notes scaffold: unknown option '%s'\n", arg.c_str());
            return 1;
        }
        else
        {
            if (haveSymbol) { std::fprintf(stderr, "kg notes scaffold: unexpected extra argument '%s'\n", arg.c_str()); return 1; }
            symbolSpec = arg;
            haveSymbol = true;
        }
    }

    if (dbPath.empty())
    {
        if (const char* env = std::getenv("KG_DB")) { dbPath = env; }
        else { dbPath = "./kg.srctrldb"; }
    }

    const int selectorCount = (all ? 1 : 0) + (!fileGlob.empty() ? 1 : 0) + (!classQName.empty() ? 1 : 0) + (haveSymbol ? 1 : 0);
    if (selectorCount != 1)
    {
        std::fprintf(stderr, "kg notes scaffold: exactly one of --all / --file GLOB / --class QNAME / SYMBOL is required\n");
        return 1;
    }

    int mask = 0;
    for (const std::string& name : kindNames)
    {
        const std::optional<int> k = kg::db::nodeKindFromString(name);
        if (!k.has_value())
        {
            std::fprintf(stderr, "kg notes scaffold: unknown --kind value '%s'\n", name.c_str());
            return 1;
        }
        mask |= *k;
    }

    std::unique_ptr<kg::db::Database> dbPtr;
    try
    {
        dbPtr = std::make_unique<kg::db::Database>(dbPath);
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "kg notes scaffold: %s\n", e.what());
        return 4;
    }
    kg::db::Database& db = *dbPtr;
    kg::query::QueryEngine engine(db);

    const std::string repoRoot = kg::db::getMeta(db.handle(), "kg_repo_root").value_or("");
    const std::string commit = kg::notes::currentCommit(db);

    std::vector<kg::notes::SymbolRow> rows;

    if (all)
    {
        rows = kg::notes::listSymbols(db, mask, {});
    }
    else if (!fileGlob.empty())
    {
        rows = kg::notes::listSymbols(db, mask, {fileGlob});
    }
    else if (!classQName.empty())
    {
        static const std::vector<int> kClassKinds = {kg::db::NodeKind::CLASS, kg::db::NodeKind::STRUCT};
        const kg::query::ResolveResult rr = kg::query::resolveSymbol(db, classQName, &kClassKinds, nullptr);
        if (rr.outcome == kg::query::ResolveOutcome::Ambiguous)
        {
            std::printf("kg notes scaffold: --class '%s' is ambiguous (%zu candidates):\n", classQName.c_str(), rr.candidates.size());
            kg::out::writeCandidateTable(std::cout, toCandidateRows(engine, rr.candidates));
            return 2;
        }
        if (rr.outcome == kg::query::ResolveOutcome::NotFound)
        {
            std::fprintf(stderr, "kg notes scaffold: --class '%s' not found.\n", classQName.c_str());
            return 3;
        }
        const int memberMask = mask != 0 ? mask : (kg::db::NodeKind::FUNCTION | kg::db::NodeKind::METHOD);
        const kg::query::BfsResult members = engine.members({rr.candidates.front().id}, memberMask);
        for (const kg::query::BfsNode& n : members.nodes)
        {
            kg::notes::SymbolRow row = kg::notes::rowForNode(db, engine, n.id, repoRoot, commit);
            if (row.nodeId != 0)
            {
                rows.push_back(std::move(row));
            }
        }
    }
    else
    {
        std::vector<int> kindFilter;
        for (const std::string& name : kindNames)
        {
            kindFilter.push_back(*kg::db::nodeKindFromString(name));
        }
        const kg::query::ResolveResult rr = kg::query::resolveSymbol(db, symbolSpec, kindFilter.empty() ? nullptr : &kindFilter, nullptr);
        if (rr.outcome == kg::query::ResolveOutcome::Ambiguous)
        {
            std::printf("kg notes scaffold: '%s' is ambiguous (%zu candidates):\n", symbolSpec.c_str(), rr.candidates.size());
            kg::out::writeCandidateTable(std::cout, toCandidateRows(engine, rr.candidates));
            return 2;
        }
        if (rr.outcome == kg::query::ResolveOutcome::NotFound)
        {
            std::fprintf(stderr, "kg notes scaffold: symbol '%s' not found.\n", symbolSpec.c_str());
            return 3;
        }
        kg::notes::SymbolRow row = kg::notes::rowForNode(db, engine, rr.candidates.front().id, repoRoot, commit);
        if (row.nodeId == 0)
        {
            std::fprintf(stderr, "kg notes scaffold: symbol '%s' has no definition location.\n", symbolSpec.c_str());
            return 3;
        }
        rows.push_back(std::move(row));
    }

    const std::string notesDir = !notesDirFlag.empty() ? notesDirFlag : resolveNotesDirDefault(dbPath);
    if (notesDir.empty())
    {
        std::fprintf(
            stderr,
            "kg notes scaffold: no notes dir given and none stored (nor a repo root) in '%s' -- pass --notes-dir DIR\n",
            dbPath.c_str());
        return 1;
    }

    kg::notes::ScaffoldOptions opts;
    opts.notesDir = notesDir;
    opts.dryRun = dryRun;
    const kg::notes::ScaffoldResult result = kg::notes::scaffoldNotes(db, rows, opts);

    if (wantJson)
    {
        nlohmann::json j;
        j["kg_version"] = KG_VERSION_STRING;
        j["schema"] = "kg.notes-scaffold/1";
        j["database"] = dbPath;
        j["notes_dir"] = std::filesystem::absolute(notesDir).string();
        j["dry_run"] = dryRun;
        j["symbols_selected"] = rows.size();
        nlohmann::json files = nlohmann::json::array();
        for (const kg::notes::ScaffoldFileResult& f : result.files)
        {
            nlohmann::json fj;
            fj["path"] = f.path;
            fj["file_created"] = f.fileCreated;
            fj["appended"] = f.appended;
            fj["skipped_existing"] = f.skippedExisting;
            files.push_back(std::move(fj));
        }
        j["files"] = std::move(files);
        nlohmann::json totals;
        totals["files_created"] = result.filesCreated;
        totals["files_touched"] = result.filesTouched;
        totals["appended"] = result.totalAppended;
        totals["skipped_existing"] = result.totalSkipped;
        j["totals"] = std::move(totals);
        std::cout << j.dump() << "\n";
    }
    else
    {
        for (const kg::notes::ScaffoldFileResult& f : result.files)
        {
            std::printf(
                "%s: %s%zu appended, %zu skipped-existing\n",
                f.path.c_str(),
                f.fileCreated ? "(new) " : "",
                f.appended,
                f.skippedExisting);
        }
        std::printf(
            "%zu symbols selected: %zu files created, %zu files touched, %zu blocks appended, %zu skipped-existing\n",
            rows.size(),
            result.filesCreated,
            result.filesTouched,
            result.totalAppended,
            result.totalSkipped);
    }
    return 0;
}

// Returns the raw (still on-disk) content of `notesDir`/`relPath`, or
// std::nullopt if it doesn't exist. Shared by `kg notes fmt` and `kg notes
// accept`'s canonical-form guard.
std::optional<std::string> readNotesFile(const std::string& notesDir, const std::string& relPath)
{
    const std::filesystem::path full = std::filesystem::path(notesDir) / relPath;
    if (!std::filesystem::is_regular_file(full))
    {
        return std::nullopt;
    }
    std::ifstream in(full, std::ios::binary);
    std::string content((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
    return content;
}

void writeNotesFile(const std::string& notesDir, const std::string& relPath, const std::string& content)
{
    const std::filesystem::path full = std::filesystem::path(notesDir) / relPath;
    std::filesystem::create_directories(full.parent_path());
    std::ofstream out(full, std::ios::binary | std::ios::trunc);
    out << content;
}

// `kg notes fmt --notes-dir DIR [--check] [--json]` -- Phase 3. Canonical
// re-render of every `*.md` under DIR (parseNotesFile -> renderFile);
// `--check` reports what would change without writing, exit 3 if anything
// would. No DB involved (same as `lint`).
int runNotesFmt(int argc, char** argv, int startIdx)
{
    std::string notesDir;
    bool checkOnly = false;
    bool wantJson = false;

    for (int i = startIdx; i < argc; ++i)
    {
        const std::string arg = argv[i];
        if (arg == "--notes-dir")
        {
            if (i + 1 >= argc) { std::fprintf(stderr, "kg notes fmt: --notes-dir requires a value\n"); return 1; }
            notesDir = argv[++i];
        }
        else if (arg == "--check") { checkOnly = true; }
        else if (arg == "--json") { wantJson = true; }
        else if (arg == "-h" || arg == "--help")
        {
            std::printf("Usage: kg notes fmt --notes-dir DIR [--check] [--json]\n");
            return 0;
        }
        else
        {
            std::fprintf(stderr, "kg notes fmt: unknown option '%s'\n", arg.c_str());
            return 1;
        }
    }
    if (notesDir.empty())
    {
        std::fprintf(stderr, "kg notes fmt: missing required option --notes-dir DIR\n");
        return 1;
    }
    if (!std::filesystem::is_directory(notesDir))
    {
        std::fprintf(stderr, "kg notes fmt: '%s' is not a directory\n", notesDir.c_str());
        return 1;
    }

    std::vector<std::string> mdFiles;
    for (const std::filesystem::directory_entry& e : std::filesystem::recursive_directory_iterator(notesDir))
    {
        if (e.is_regular_file() && e.path().extension() == ".md")
        {
            mdFiles.push_back(e.path().string());
        }
    }
    std::sort(mdFiles.begin(), mdFiles.end());

    std::vector<std::string> changed;
    for (const std::string& full : mdFiles)
    {
        const std::string relPath = std::filesystem::relative(full, notesDir).generic_string();
        std::ifstream in(full, std::ios::binary);
        std::string content((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
        const kg::notes::NotesFile parsed = kg::notes::parseNotesFile(content, relPath, false);
        const std::string rendered = kg::notes::renderFile(parsed);
        if (rendered != content)
        {
            changed.push_back(relPath);
            if (!checkOnly)
            {
                writeNotesFile(notesDir, relPath, rendered);
            }
        }
    }

    if (wantJson)
    {
        nlohmann::json j;
        j["kg_version"] = KG_VERSION_STRING;
        j["schema"] = "kg.notes-fmt/1";
        j["notes_dir"] = notesDir;
        j["check"] = checkOnly;
        j["files"] = mdFiles.size();
        j["changed"] = changed;
        std::cout << j.dump() << "\n";
    }
    else
    {
        for (const std::string& p : changed)
        {
            std::printf("%s%s\n", checkOnly ? "would reformat: " : "reformatted: ", p.c_str());
        }
        std::printf("%zu files, %zu %s\n", mdFiles.size(), changed.size(), checkOnly ? "would change" : "reformatted");
    }
    return (checkOnly && !changed.empty()) ? 3 : 0;
}

// `kg notes accept -d DB (SYMBOL | --all | --file SRC-GLOB) [--json]` --
// Phase 3. Rewrites ONLY the `kg-hash`/`kg-commit` metadata comment of the
// matching stale block(s) (every stale note for --all/--file; the resolved
// SYMBOL's note unconditionally) to the current body hash/commit, refusing
// (per file) any notes file that isn't already in its renderFile() fixed
// point ("run `kg notes fmt`" -- see docs/NOTES_DESIGN.md). Re-runs
// syncNotes() afterward so the cache's `stale` flags reflect the rewrite.
// Exit codes: 0 ok, 1 usage, 3 nothing to accept (SYMBOL has no note), 4 db
// error / a file needed reformatting first (reported, not fatal to the
// others).
int runNotesAccept(int argc, char** argv, int startIdx)
{
    std::string dbPath;
    std::string notesDirFlag;
    bool all = false;
    std::string fileGlob;
    std::string symbolSpec;
    bool haveSymbol = false;
    bool wantJson = false;

    for (int i = startIdx; i < argc; ++i)
    {
        const std::string arg = argv[i];
        if (arg == "-d" || arg == "--db")
        {
            if (i + 1 >= argc) { std::fprintf(stderr, "kg notes accept: '%s' requires a value\n", arg.c_str()); return 1; }
            dbPath = argv[++i];
        }
        else if (arg == "--notes-dir")
        {
            if (i + 1 >= argc) { std::fprintf(stderr, "kg notes accept: --notes-dir requires a value\n"); return 1; }
            notesDirFlag = argv[++i];
        }
        else if (arg == "--all") { all = true; }
        else if (arg == "--file")
        {
            if (i + 1 >= argc) { std::fprintf(stderr, "kg notes accept: --file requires a value\n"); return 1; }
            fileGlob = argv[++i];
        }
        else if (arg == "--json") { wantJson = true; }
        else if (arg == "-h" || arg == "--help")
        {
            std::printf("Usage: kg notes accept -d DB (SYMBOL | --all | --file SRC-GLOB) [--json]\n");
            return 0;
        }
        else if (!arg.empty() && arg[0] == '-' && arg != "-")
        {
            std::fprintf(stderr, "kg notes accept: unknown option '%s'\n", arg.c_str());
            return 1;
        }
        else
        {
            if (haveSymbol) { std::fprintf(stderr, "kg notes accept: unexpected extra argument '%s'\n", arg.c_str()); return 1; }
            symbolSpec = arg;
            haveSymbol = true;
        }
    }

    if (dbPath.empty())
    {
        if (const char* env = std::getenv("KG_DB")) { dbPath = env; }
        else { dbPath = "./kg.srctrldb"; }
    }
    const int selectorCount = (all ? 1 : 0) + (!fileGlob.empty() ? 1 : 0) + (haveSymbol ? 1 : 0);
    if (selectorCount != 1)
    {
        std::fprintf(stderr, "kg notes accept: exactly one of SYMBOL / --all / --file GLOB is required\n");
        return 1;
    }

    std::unique_ptr<kg::db::Database> dbPtr;
    try
    {
        dbPtr = std::make_unique<kg::db::Database>(dbPath);
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "kg notes accept: %s\n", e.what());
        return 4;
    }
    kg::db::Database& db = *dbPtr;

    const std::string notesDir = !notesDirFlag.empty() ? notesDirFlag : resolveNotesDirDefault(dbPath);
    if (notesDir.empty())
    {
        std::fprintf(stderr, "kg notes accept: no notes dir given and none stored (nor a repo root) -- pass --notes-dir DIR\n");
        return 1;
    }

    const kg::notes::NoteLookup lookup = kg::notes::openNotes(db);

    // Determine which serialized_names to accept.
    std::unordered_set<std::string> targets;
    if (all)
    {
        for (const auto& [sn, note] : lookup.bySerializedName)
        {
            if (note.stale) { targets.insert(sn); }
        }
    }
    else if (!fileGlob.empty())
    {
        for (const auto& [sn, note] : lookup.bySerializedName)
        {
            if (!note.stale) { continue; }
            std::string sourcePath = note.notesPath;
            if (sourcePath.size() > 3 && sourcePath.compare(sourcePath.size() - 3, 3, ".md") == 0)
            {
                sourcePath = sourcePath.substr(0, sourcePath.size() - 3);
            }
            if (kg::common::glob::match(fileGlob, sourcePath))
            {
                targets.insert(sn);
            }
        }
    }
    else
    {
        const kg::query::ResolveResult rr = kg::query::resolveSymbol(db, symbolSpec, nullptr, nullptr);
        if (rr.outcome == kg::query::ResolveOutcome::Ambiguous)
        {
            kg::query::QueryEngine engine(db);
            std::printf("kg notes accept: '%s' is ambiguous (%zu candidates):\n", symbolSpec.c_str(), rr.candidates.size());
            kg::out::writeCandidateTable(std::cout, toCandidateRows(engine, rr.candidates));
            return 2;
        }
        if (rr.outcome == kg::query::ResolveOutcome::NotFound)
        {
            std::fprintf(stderr, "kg notes accept: symbol '%s' not found.\n", symbolSpec.c_str());
            return 3;
        }
        const auto it = lookup.bySerializedName.find(rr.candidates.front().serializedName);
        if (it == lookup.bySerializedName.end())
        {
            std::fprintf(stderr, "kg notes accept: symbol '%s' has no note.\n", symbolSpec.c_str());
            return 3;
        }
        targets.insert(it->first);
    }

    if (targets.empty())
    {
        if (wantJson)
        {
            nlohmann::json j;
            j["kg_version"] = KG_VERSION_STRING;
            j["schema"] = "kg.notes-accept/1";
            j["database"] = dbPath;
            j["accepted"] = nlohmann::json::array();
            j["refused_files"] = nlohmann::json::array();
            std::cout << j.dump() << "\n";
        }
        else
        {
            std::printf("kg notes accept: nothing to accept (0 stale/matching notes)\n");
        }
        return 0;
    }

    // Group targets by notes file.
    std::unordered_map<std::string, std::vector<std::string>> byFile;  // notesPath -> [serialized_name...]
    for (const std::string& sn : targets)
    {
        byFile[lookup.bySerializedName.at(sn).notesPath].push_back(sn);
    }

    const std::string commit = kg::notes::currentCommit(db);
    std::vector<std::string> accepted;            // serialized_name (stable identity, used in --json)
    std::vector<std::string> acceptedDisplay;      // qualified_name (used in the text-mode listing)
    std::vector<std::string> refusedFiles;

    for (const auto& [notesPath, sns] : byFile)
    {
        const std::optional<std::string> contentOpt = readNotesFile(notesDir, notesPath);
        if (!contentOpt.has_value())
        {
            refusedFiles.push_back(notesPath);
            continue;
        }
        kg::notes::NotesFile parsed = kg::notes::parseNotesFile(*contentOpt, notesPath, false);
        const std::string canonical = kg::notes::renderFile(parsed);
        if (canonical != *contentOpt)
        {
            std::fprintf(
                stderr,
                "kg notes accept: '%s' is not in canonical form; run `kg notes fmt` first -- skipped\n",
                notesPath.c_str());
            refusedFiles.push_back(notesPath);
            continue;
        }

        const std::unordered_set<std::string> wanted(sns.begin(), sns.end());
        bool anyChanged = false;
        for (kg::notes::NoteBlock& b : parsed.blocks)
        {
            const kg::notes::MatchOutcome mo = kg::notes::matchBlock(db, b);
            if (mo.kind != kg::notes::MatchKind::Matched || wanted.find(mo.serializedName) == wanted.end())
            {
                continue;
            }
            b.bodyHash = kg::notes::bodyHashForNode(db, mo.nodeId);
            b.commit = commit;
            accepted.push_back(mo.serializedName);
            acceptedDisplay.push_back(mo.qualifiedName);
            anyChanged = true;
        }
        if (anyChanged)
        {
            writeNotesFile(notesDir, notesPath, kg::notes::renderFile(parsed));
        }
    }

    // Refresh the cache so `stale` clears for what was just accepted.
    try
    {
        kg::notes::syncNotes(dbPath, notesDir, false);
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "kg notes accept: warning: post-accept sync failed: %s\n", e.what());
    }

    std::sort(accepted.begin(), accepted.end());
    std::sort(acceptedDisplay.begin(), acceptedDisplay.end());
    std::sort(refusedFiles.begin(), refusedFiles.end());

    if (wantJson)
    {
        nlohmann::json j;
        j["kg_version"] = KG_VERSION_STRING;
        j["schema"] = "kg.notes-accept/1";
        j["database"] = dbPath;
        j["accepted"] = accepted;
        j["refused_files"] = refusedFiles;
        std::cout << j.dump() << "\n";
    }
    else
    {
        for (const std::string& qn : acceptedDisplay)
        {
            std::printf("accepted: %s\n", qn.c_str());
        }
        std::printf("%zu accepted, %zu file(s) refused (not canonical / unreadable)\n", accepted.size(), refusedFiles.size());
    }
    return refusedFiles.empty() ? 0 : 4;
}

// `kg notes VERB ...` dispatch. Only `lint` is implemented in Phase 1 (see
// docs/NOTES_DESIGN.md's "Phases" section) -- scaffold/sync/show/status/
// accept are listed but print "not implemented in this build" (exit 1)
// until their phases land.
int runNotes(int argc, char** argv, int startIdx)
{
    static const char* kUsage =
        "Usage: kg notes VERB ...\n"
        "  lint     --notes-dir DIR [--strict] [--json]\n"
        "  sync     -d DB [--notes-dir DIR] [--strict] [--json]\n"
        "  show     -d DB SYMBOL [--section brief|what|how|signature|pre|post|bugs|suggestions] [--json]\n"
        "  status   -d DB [--stale] [--unfilled] [--unanchored] [--problems] [--json]\n"
        "  symbols  -d DB [--kind K]... [--file GLOB] [--json]\n"
        "  scaffold -d DB [--notes-dir DIR] (--all | --file GLOB | --class QNAME | SYMBOL)\n"
        "                 [--kind K]... [--dry-run] [--json]\n"
        "  accept   -d DB (SYMBOL | --all | --file GLOB) [--json]\n"
        "  fmt      --notes-dir DIR [--check] [--json]\n";

    if (startIdx >= argc)
    {
        std::fprintf(stderr, "%s", kUsage);
        return 1;
    }
    const std::string verb = argv[startIdx];
    if (verb == "lint")
    {
        return runNotesLint(argc, argv, startIdx + 1);
    }
    if (verb == "sync")
    {
        return runNotesSync(argc, argv, startIdx + 1);
    }
    if (verb == "show")
    {
        return runNotesShow(argc, argv, startIdx + 1);
    }
    if (verb == "status")
    {
        return runNotesStatus(argc, argv, startIdx + 1);
    }
    if (verb == "symbols")
    {
        return runNotesSymbols(argc, argv, startIdx + 1);
    }
    if (verb == "scaffold")
    {
        return runNotesScaffold(argc, argv, startIdx + 1);
    }
    if (verb == "accept")
    {
        return runNotesAccept(argc, argv, startIdx + 1);
    }
    if (verb == "fmt")
    {
        return runNotesFmt(argc, argv, startIdx + 1);
    }
    if (verb == "-h" || verb == "--help")
    {
        std::printf("%s", kUsage);
        return 0;
    }
    std::fprintf(stderr, "kg notes: unknown verb '%s'\n", verb.c_str());
    return 1;
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
    std::vector<std::string> onlyCategories;

    for (int i = startIdx; i < argc; ++i)
    {
        const std::string arg = argv[i];
        if (arg == "--json")
        {
            wantJson = true;
        }
        else if (arg == "--only")
        {
            if (i + 1 >= argc)
            {
                std::fprintf(stderr, "kg dbdiff: error: --only requires a value\n");
                return 4;
            }
            onlyCategories.push_back(argv[++i]);
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
        std::fprintf(stderr, "usage: kg dbdiff A B [--json] [--limit N] [--ignore shared-header-locations] [--only CATEGORY]\n");
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

    const kg::update::SnapshotDiff diff = kg::update::diffSnapshots(*a, *b, onlyCategories);
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
    if (first == "notes")
    {
        return runNotes(argc, argv, 2);
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
