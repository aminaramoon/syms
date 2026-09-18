#include "kg/index/IndexDriver.h"

#include <chrono>
#include <cstdio>
#include <cstdint>
#include <filesystem>
#include <sstream>
#include <vector>

#include <clang-c/Index.h>
#include <nlohmann/json.hpp>

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

int runIndex(const IndexRunOptions& options)
{
    if (std::filesystem::exists(options.outputPath))
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
    try
    {
        entries = load(options.compileCommandsPath, options.dbOptions);
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "kg index: %s\n", e.what());
        return 1;
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

        best.batch.stats.tuFile = entry.file;
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

    return summary.tusWithContent > 0 ? 0 : 5;
}

}  // namespace kg::index
