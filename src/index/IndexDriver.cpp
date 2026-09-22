#include "kg/index/IndexDriver.h"

#include <chrono>
#include <cstdio>
#include <cstdint>
#include <filesystem>
#include <sstream>
#include <unordered_map>
#include <vector>

#include <clang-c/Index.h>
#include <nlohmann/json.hpp>

#include <sqlite3.h>

#include "kg/common/Paths.h"
#include "kg/db/Meta.h"
#include "kg/db/WritableDb.h"
#include "kg/index/AstWalker.h"
#include "kg/index/ClangUtil.h"
#include "kg/index/CudaSupport.h"
#include "kg/index/SrctrldbBackend.h"
#include "kg/index/TuParser.h"
#include "kg/model/FactSink.h"
#include "kg/model/IndexBackend.h"

namespace kg::index
{

namespace
{

bool endsWithCu(const std::string& path)
{
    return path.size() >= 3 && path.compare(path.size() - 3, 3, ".cu") == 0;
}

uint64_t millisSince(std::chrono::steady_clock::time_point start)
{
    return static_cast<uint64_t>(
        std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::steady_clock::now() - start).count());
}

// Writes `prov` (if set) and one kg_tu row per indexed `entries` element
// into `outputPath`'s `meta`/`kg_tu` tables. Called once, after finish()
// has already committed the writer's own transaction and created
// kg_symbol_def/kg_tu -- this opens a SEPARATE short-lived WritableDb
// transaction rather than trying to thread one through SrctrldbBackend.
void writeProvenanceAndTus(
    const std::string& outputPath,
    const std::optional<Provenance>& prov,
    const std::vector<CompileEntry>& entries,
    const std::vector<std::string>& tuStatuses)
{
    if (!prov.has_value() && entries.empty())
    {
        return;
    }

    kg::db::WritableDb db(outputPath, /*foreignKeys=*/false);
    db.begin();

    db.exec(kKgTuDdl);

    if (prov.has_value())
    {
        writeProvenance(db, *prov);
    }

    // file.path (from clang's clang_getFileName()) and entry.file (the CDB
    // entry's `file` field, post-remap) usually agree textually but can
    // differ on symlinks or a literal "./" prefix -- an exact string `=`
    // match would then silently miss and leave kg_tu.file_node_id NULL.
    // Load every file.path once, normalized, and match both sides through
    // the same normalization instead of a per-entry exact-match SELECT.
    std::unordered_map<std::string, int> fileNodeIdByNormalizedPath;
    db.query("SELECT id, path FROM file", [&](sqlite3_stmt* stmt) {
        const int id = sqlite3_column_int(stmt, 0);
        const char* pathText = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 1));
        if (pathText != nullptr)
        {
            fileNodeIdByNormalizedPath[kg::common::paths::normalize(pathText)] = id;
        }
    });

    static const char* kUpsertTuSql =
        "INSERT INTO kg_tu(path, args_hash, file_node_id, status) VALUES(?1, ?2, ?3, ?4) "
        "ON CONFLICT(path) DO UPDATE SET args_hash = excluded.args_hash, file_node_id = excluded.file_node_id, "
        "status = excluded.status";
    for (std::size_t i = 0; i < entries.size(); ++i)
    {
        const auto& entry = entries[i];
        int fileNodeId = 0;
        bool found = false;
        const auto it = fileNodeIdByNormalizedPath.find(kg::common::paths::normalize(entry.file));
        if (it != fileNodeIdByNormalizedPath.end())
        {
            fileNodeId = it->second;
            found = true;
        }

        const std::string hash = computeArgsHash(entry);
        // tuStatuses is index-aligned with entries (built by runIndex()'s
        // main loop); default to "ok" defensively if a caller ever passes a
        // shorter vector (should not happen in practice).
        const std::string status = i < tuStatuses.size() ? tuStatuses[i] : std::string("ok");
        db.execBound(
            kUpsertTuSql,
            [&](sqlite3_stmt* stmt) {
                sqlite3_bind_text(stmt, 1, entry.file.c_str(), -1, SQLITE_TRANSIENT);
                sqlite3_bind_text(stmt, 2, hash.c_str(), -1, SQLITE_TRANSIENT);
                if (found)
                {
                    sqlite3_bind_int(stmt, 3, fileNodeId);
                }
                else
                {
                    sqlite3_bind_null(stmt, 3);
                }
                sqlite3_bind_text(stmt, 4, status.c_str(), -1, SQLITE_TRANSIENT);
            });
    }

    db.commit();
}

// Records a single-file, single-error "couldn't even try" batch (used for
// exceptions from sanitize(), a --cuda-mode skip, or clang_parseTranslationUnit2
// producing no usable TU at all).
kg::model::TuFactBatch minimalFailureBatch(const std::string& path, const std::string& message, bool fatal)
{
    kg::model::BatchSink sink;
    kg::model::FileFact ff;
    ff.path = path;
    ff.language = "cpp";
    ff.indexed = true;
    const kg::model::LocalId fid = sink.file(std::move(ff));
    kg::model::SourceRange r;
    r.file = fid;
    r.startLine = r.endLine = 1;
    r.startCol = r.endCol = 1;
    sink.error(message, fatal, &r);
    return sink.take();
}

void appendDiagnostics(const ParsedTU& parsed, FileRegistry& files, kg::model::FactSink& sink, bool& anyFatal)
{
    for (const auto& d : parsed.diagnostics)
    {
        kg::model::LocalId fileId = kg::model::kNoLocal;
        if (d.hasLocation && !d.file.empty())
        {
            CXFile file = clang_getFile(parsed.tu.get(), d.file.c_str());
            if (file != nullptr)
            {
                const FileRegistry::Info& info = files.infoFor(parsed.tu.get(), file);
                kg::model::FileFact ff;
                ff.path = info.path;
                ff.language = "cpp";
                ff.indexed = info.indexed;
                ff.system = info.system;
                fileId = sink.file(std::move(ff));
            }
        }
        kg::model::SourceRange range;
        range.file = fileId;
        range.startLine = range.endLine = d.line;
        range.startCol = range.endCol = d.col;
        sink.error(d.message, d.fatal, fileId != kg::model::kNoLocal ? &range : nullptr);
        if (d.fatal)
        {
            anyFatal = true;
        }
    }
}

struct RungResult
{
    kg::model::TuFactBatch batch;
    bool contentOk = false;
    uint64_t parseMs = 0;
    uint64_t walkMs = 0;
    uint32_t diagErrors = 0;
    uint32_t diagFatal = 0;
};

// One independent attempt: sanitize (forcing `rung`) -> parse -> walk. Never
// merges with any other attempt's facts -- the CUDA retry loop in runIndex()
// picks exactly one winning attempt's batch (or synthesizes its own
// all-rungs-failed batch) rather than combining partial results.
RungResult tryRung(const CompileEntry& entry, SanitizerOptions sanitizerOptions, cuda::CudaMode rung, const FileRegistryOptions& fileRegOptions)
{
    sanitizerOptions.cudaMode = rung;

    RungResult result;

    SanitizeResult sanResult;
    try
    {
        sanResult = sanitize(entry, sanitizerOptions);
    }
    catch (const std::exception& e)
    {
        result.batch = minimalFailureBatch(entry.file, std::string("kg: failed to sanitize compiler arguments: ") + e.what(), true);
        result.contentOk = false;
        return result;
    }

    if (sanResult.skipped)
    {
        result.batch = minimalFailureBatch(entry.file, "kg: translation unit skipped (--cuda-mode skip)", false);
        result.contentOk = false;
        return result;
    }

    kg::model::BatchSink sink;
    FileRegistry files(fileRegOptions);

    const auto parseStart = std::chrono::steady_clock::now();
    ParsedTU parsed = parseTU(sanResult.args);
    result.parseMs = millisSince(parseStart);
    result.diagErrors = parsed.diagErrors;
    result.diagFatal = parsed.diagFatal;

    if (!parsed.ok)
    {
        result.batch = minimalFailureBatch(
            entry.file, "kg: clang_parseTranslationUnit2 failed to produce a usable translation unit", true);
        result.contentOk = false;
        return result;
    }

    bool anyFatal = false;
    const auto walkStart = std::chrono::steady_clock::now();
    AstWalker walker(parsed.tu.get(), files, sink);
    walker.run();
    appendDiagnostics(parsed, files, sink, anyFatal);
    result.walkMs = millisSince(walkStart);

    if (!sink.batch().hasContent())
    {
        const std::string mainPath = clangutil::toString(clang_getTranslationUnitSpelling(parsed.tu.get()));
        kg::model::FileFact ff;
        ff.path = mainPath.empty() ? entry.file : mainPath;
        ff.language = "cpp";
        ff.indexed = true;
        const kg::model::LocalId fid = sink.file(std::move(ff));
        if (sink.batch().errors.empty())
        {
            std::ostringstream oss;
            oss << "kg: translation unit produced no content (" << (parsed.diagErrors + parsed.diagFatal) << " error diagnostics)";
            kg::model::SourceRange r;
            r.file = fid;
            r.startLine = r.endLine = 1;
            r.startCol = r.endCol = 1;
            sink.error(oss.str(), false, &r);
        }
    }

    result.batch = sink.take();
    result.contentOk = result.batch.hasContent() && !anyFatal;
    return result;
}

}  // namespace

int runIndex(const IndexRunOptions& options, kg::model::IndexSummary* summaryOut)
{
    if (!options.appendToExisting && std::filesystem::exists(options.outputPath))
    {
        if (!options.force)
        {
            std::fprintf(
                stderr, "kg index: output '%s' already exists (use --force to overwrite)\n", options.outputPath.c_str());
            return 1;
        }
        std::error_code ec;
        std::filesystem::remove(options.outputPath, ec);
        std::filesystem::remove(options.outputPath + "-journal", ec);
        std::filesystem::path proj(options.outputPath);
        proj.replace_extension(".srctrlprj");
        std::filesystem::remove(proj, ec);
    }

    std::vector<CompileEntry> entries;
    if (options.presetEntries.has_value())
    {
        entries = *options.presetEntries;
    }
    else
    {
        try
        {
            entries = load(options.compileCommandsPath, options.dbOptions);
        }
        catch (const std::exception& e)
        {
            std::fprintf(stderr, "kg index: %s\n", e.what());
            return 1;
        }
    }

    if (options.limit >= 0 && static_cast<std::size_t>(options.limit) < entries.size())
    {
        entries.resize(static_cast<std::size_t>(options.limit));
    }

    SrctrldbBackend backend;
    backend.setNoVacuum(options.noVacuum);
    backend.setVerbose(options.verbose);

    std::string err;
    if (!backend.begin(options.outputPath, &err))
    {
        std::fprintf(stderr, "kg index: failed to open output database '%s': %s\n", options.outputPath.c_str(), err.c_str());
        return 1;
    }

    FileRegistryOptions fileRegOptions;
    fileRegOptions.includeGlobs = options.dbOptions.includeGlobs;
    fileRegOptions.excludeGlobs = options.dbOptions.excludeGlobs;
    fileRegOptions.systemFileRows = options.systemFileRows;

    const auto startTime = std::chrono::steady_clock::now();

    const uint32_t tusTotal = static_cast<uint32_t>(entries.size());
    uint32_t tusAttempted = 0, tusWithContent = 0, tusFailed = 0;
    std::vector<std::string> failedTus;
    std::vector<std::string> tuStatuses;
    tuStatuses.reserve(entries.size());

    static const cuda::CudaMode kRungs[] = {cuda::CudaMode::Full, cuda::CudaMode::NoWrapper, cuda::CudaMode::HostOnly};

    for (const auto& entry : entries)
    {
        ++tusAttempted;
        const bool isCuda = endsWithCu(entry.file);
        const cuda::CudaMode requestedMode = options.sanitizerOptions.cudaMode;

        RungResult best;
        uint32_t rungIndex = 0;

        if (isCuda && requestedMode == cuda::CudaMode::Auto)
        {
            bool ok = false;
            for (std::size_t i = 0; i < 3; ++i)
            {
                RungResult r = tryRung(entry, options.sanitizerOptions, kRungs[i], fileRegOptions);
                best = std::move(r);
                if (best.contentOk)
                {
                    rungIndex = static_cast<uint32_t>(i + 1);
                    ok = true;
                    break;
                }
            }
            if (!ok)
            {
                best.batch = minimalFailureBatch(
                    entry.file,
                    "kg: all CUDA rungs (full, nowrapper, host-only) failed to produce content for this translation unit",
                    true);
                best.contentOk = false;
                rungIndex = 4;
            }
        }
        else
        {
            // Non-CUDA file, or an explicit (non-auto) --cuda-mode: a
            // single, direct attempt -- no retry.
            best = tryRung(entry, options.sanitizerOptions, requestedMode, fileRegOptions);
            rungIndex = 0;
        }

        best.batch.stats.tuFile = kg::common::paths::normalize(entry.file);
        best.batch.stats.parsedRungIndex = rungIndex;
        best.batch.stats.parsedOk = best.contentOk;
        best.batch.stats.parseMs = best.parseMs;
        best.batch.stats.walkMs = best.walkMs;
        best.batch.stats.diagErrors = best.diagErrors;
        best.batch.stats.diagFatal = best.diagFatal;

        const bool hasContent = best.batch.hasContent();
        if (hasContent)
        {
            ++tusWithContent;
        }
        else
        {
            ++tusFailed;
            failedTus.push_back(entry.file);
        }

        // Per-TU status persisted into kg_tu.status (see Provenance.h) --
        // "failed" (no content), "partial" (content but a fatal diagnostic,
        // or a CUDA run that only succeeded on a degraded fallback rung),
        // "ok" otherwise. rungIndex is only meaningful for the CUDA
        // Auto-mode retry ladder (0 for non-CUDA / non-auto): 1 = Full
        // (not degraded), 2/3 = NoWrapper/HostOnly (degraded fallback).
        if (!hasContent)
        {
            tuStatuses.push_back("failed");
        }
        else if (best.batch.stats.diagFatal > 0 || rungIndex >= 2)
        {
            tuStatuses.push_back("partial");
        }
        else
        {
            tuStatuses.push_back("ok");
        }

        std::string consumeErr;
        if (!backend.consume(best.batch, &consumeErr))
        {
            std::fprintf(stderr, "kg index: failed to write facts for '%s': %s\n", entry.file.c_str(), consumeErr.c_str());
        }

        if (!options.quiet)
        {
            std::string line = entry.file + ": " + (hasContent ? "ok" : "no-content");
            if (rungIndex > 0)
            {
                line += " (cuda rung " + std::to_string(rungIndex) + ")";
            }
            std::fprintf(stderr, "%s\n", line.c_str());
        }
    }

    kg::model::IndexSummary summary;
    if (!backend.finish(summary, &err))
    {
        std::fprintf(stderr, "kg index: failed to finalize output database: %s\n", err.c_str());
        return 1;
    }

    try
    {
        writeProvenanceAndTus(options.outputPath, options.provenance, entries, tuStatuses);
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "kg index: failed to write provenance/kg_tu rows: %s\n", e.what());
        return 1;
    }

    summary.tusTotal = tusTotal;
    summary.tusAttempted = tusAttempted;
    summary.tusWithContent = tusWithContent;
    summary.tusFailed = tusFailed;
    summary.failedTus = failedTus;
    summary.elapsedMs = millisSince(startTime);

    std::fprintf(
        stderr,
        "kg index: %u/%u TUs produced content (%u failed), %u symbols, %u references, %u files, %u errors, %u name "
        "collisions, %llu ms\n",
        summary.tusWithContent,
        summary.tusTotal,
        summary.tusFailed,
        summary.symbols,
        summary.references,
        summary.files,
        summary.errors,
        summary.nameCollisions,
        static_cast<unsigned long long>(summary.elapsedMs));

    if (options.jsonSummary)
    {
        // kg.index-summary/1 -- see docs/JSON_SCHEMA.md. Reshapes the same
        // IndexSummary data Phase 3 already printed (just as a flat,
        // ad-hoc-keyed object) into the Phase 5 envelope convention shared
        // by every other `kg`/`kg query` JSON schema (kg_version + schema +
        // ...); the underlying indexing logic above is untouched.
        nlohmann::json j;
        j["kg_version"] = options.kgVersion;
        j["schema"] = "kg.index-summary/1";
        j["database"] = options.outputPath;
        j["counts"] = {
            {"tus_total", summary.tusTotal},
            {"tus_attempted", summary.tusAttempted},
            {"tus_with_content", summary.tusWithContent},
            {"tus_failed", summary.tusFailed},
            {"symbols", summary.symbols},
            {"references", summary.references},
            {"files", summary.files},
            {"errors", summary.errors},
            {"name_collisions", summary.nameCollisions},
        };
        j["elapsed_ms"] = summary.elapsedMs;
        j["failed_tus"] = summary.failedTus;
        std::printf("%s\n", j.dump().c_str());
    }

    if (summaryOut != nullptr)
    {
        *summaryOut = summary;
    }

    return summary.tusWithContent > 0 ? 0 : 5;
}

}  // namespace kg::index
