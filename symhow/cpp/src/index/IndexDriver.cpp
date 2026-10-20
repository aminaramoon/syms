#include "symhow/index/IndexDriver.h"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <csignal>
#include <cstdio>
#include <cstdint>
#include <filesystem>
#include <mutex>
#include <sstream>
#include <thread>
#include <unordered_map>
#include <vector>

#include <clang-c/Index.h>
#include <nlohmann/json.hpp>

#include <sqlite3.h>

#include "symhow/common/Paths.h"
#include "symhow/db/Database.h"
#include "symhow/db/Meta.h"
#include "symhow/db/WritableDb.h"
#include "symhow/index/AstWalker.h"
#include "symhow/index/ClangUtil.h"
#include "symhow/index/CudaSupport.h"
#include "symhow/index/Progress.h"
#include "symhow/index/Provenance.h"
#include "symhow/index/SrctrldbBackend.h"
#include "symhow/index/TuParser.h"
#include "symhow/model/FactSink.h"
#include "symhow/model/IndexBackend.h"

namespace symhow::index
{

namespace
{

// Set by the SIGINT/SIGTERM handler below -- a signal-safe sig_atomic_t
// flag, no I/O or allocation in the handler itself. Checked once per
// consumed TU by the (single) consumer thread in runIndex(); real signal
// delivery is therefore best-effort ("interrupted after K/N TUs", not an
// exact K) -- --stop-after (checked the same way, but driven by a plain
// counter instead of a signal) is what gives tests an EXACT, deterministic
// interruption point.
volatile std::sig_atomic_t g_interruptRequested = 0;

extern "C" void handleInterruptSignal(int)
{
    g_interruptRequested = 1;
}

// Installed once per runIndex() call (idempotent: re-installing the same
// handler is harmless, and this process only ever calls runIndex() once in
// normal CLI use -- tests that call it repeatedly in-process just reinstall
// the same trivial handler each time).
void installInterruptHandlers()
{
    std::signal(SIGINT, handleInterruptSignal);
    std::signal(SIGTERM, handleInterruptSignal);
}

bool endsWithCu(const std::string& path)
{
    return path.size() >= 3 && path.compare(path.size() - 3, 3, ".cu") == 0;
}

uint64_t millisSince(std::chrono::steady_clock::time_point start)
{
    return static_cast<uint64_t>(
        std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::steady_clock::now() - start).count());
}

// Writes `prov` into `outputPath`'s `meta` table (plus
// `symhow_index_in_progress="1"`), in its own tiny transaction on a SEPARATE
// short-lived WritableDb connection -- called right after
// SrctrldbBackend::begin() (which opens the file and creates the schema,
// but doesn't start the writer's transaction), BEFORE any TU is consumed.
// This is what makes provenance (and the in-progress marker `symhow update`
// keys its "resume" behavior off) durable even if the run is interrupted
// before a single TU finishes. No-op if `prov` isn't set (some low-level
// callers, e.g. unit tests, run without provenance at all -- symhow_tu rows are
// still written per-checkpoint regardless, see SrctrldbBackend::checkpoint).
void writeProvenanceEarly(const std::string& outputPath, const std::optional<Provenance>& prov)
{
    if (!prov.has_value())
    {
        return;
    }
    symhow::db::WritableDb db(outputPath, /*foreignKeys=*/false);
    db.begin();
    writeProvenance(db, *prov);
    symhow::db::setMeta(db, "symhow_index_in_progress", "1");
    db.commit();
}

// Records a single-file, single-error "couldn't even try" batch (used for
// exceptions from sanitize(), a --cuda-mode skip, or clang_parseTranslationUnit2
// producing no usable TU at all).
symhow::model::TuFactBatch minimalFailureBatch(const std::string& path, const std::string& message, bool fatal)
{
    symhow::model::BatchSink sink;
    symhow::model::FileFact ff;
    ff.path = path;
    ff.language = "cpp";
    ff.indexed = true;
    const symhow::model::LocalId fid = sink.file(std::move(ff));
    symhow::model::SourceRange r;
    r.file = fid;
    r.startLine = r.endLine = 1;
    r.startCol = r.endCol = 1;
    sink.error(message, fatal, &r);
    return sink.take();
}

void appendDiagnostics(const ParsedTU& parsed, FileRegistry& files, symhow::model::FactSink& sink, bool& anyFatal)
{
    for (const auto& d : parsed.diagnostics)
    {
        symhow::model::LocalId fileId = symhow::model::kNoLocal;
        if (d.hasLocation && !d.file.empty())
        {
            CXFile file = clang_getFile(parsed.tu.get(), d.file.c_str());
            if (file != nullptr)
            {
                const FileRegistry::Info& info = files.infoFor(parsed.tu.get(), file);
                symhow::model::FileFact ff;
                ff.path = info.path;
                ff.language = "cpp";
                ff.indexed = info.indexed;
                ff.system = info.system;
                ff.contentHash = info.contentHash;
                fileId = sink.file(std::move(ff));
            }
        }
        symhow::model::SourceRange range;
        range.file = fileId;
        range.startLine = range.endLine = d.line;
        range.startCol = range.endCol = d.col;
        sink.error(d.message, d.fatal, fileId != symhow::model::kNoLocal ? &range : nullptr);
        if (d.fatal)
        {
            anyFatal = true;
        }
    }
}

struct RungResult
{
    symhow::model::TuFactBatch batch;
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
        result.batch = minimalFailureBatch(entry.file, std::string("symhow: failed to sanitize compiler arguments: ") + e.what(), true);
        result.contentOk = false;
        return result;
    }

    if (sanResult.skipped)
    {
        result.batch = minimalFailureBatch(entry.file, "symhow: translation unit skipped (--cuda-mode skip)", false);
        result.contentOk = false;
        return result;
    }

    symhow::model::BatchSink sink;
    FileRegistry files(fileRegOptions);

    const auto parseStart = std::chrono::steady_clock::now();
    ParsedTU parsed = parseTU(sanResult.args);
    result.parseMs = millisSince(parseStart);
    result.diagErrors = parsed.diagErrors;
    result.diagFatal = parsed.diagFatal;

    if (!parsed.ok)
    {
        result.batch = minimalFailureBatch(
            entry.file, "symhow: clang_parseTranslationUnit2 failed to produce a usable translation unit", true);
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
        symhow::model::FileFact ff;
        ff.path = mainPath.empty() ? entry.file : mainPath;
        ff.language = "cpp";
        ff.indexed = true;
        const symhow::model::LocalId fid = sink.file(std::move(ff));
        if (sink.batch().errors.empty())
        {
            std::ostringstream oss;
            oss << "symhow: translation unit produced no content (" << (parsed.diagErrors + parsed.diagFatal) << " error diagnostics)";
            symhow::model::SourceRange r;
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

// Outcome of fully processing one CompileEntry: sanitize -> (CUDA retry
// ladder if applicable) -> parse -> walk. Self-contained (a fresh CXIndex,
// FileRegistry and BatchSink per call) and side-effect-free w.r.t. any
// shared state, so it is safe to run on a worker thread (see runIndexParallel
// below) -- the only shared-state step left, SrctrldbBackend::consume(), is
// deliberately NOT part of this function.
struct TuOutcome
{
    symhow::model::TuFactBatch batch;
    uint32_t rungIndex = 0;
    bool hasContent = false;
    std::string status;  // persisted symhow_tu.status: "ok" / "partial" / "failed"
    double extractSeconds = 0.0;
};

TuOutcome processEntry(const CompileEntry& entry, const IndexRunOptions& options, const FileRegistryOptions& fileRegOptions)
{
    const auto entryStart = std::chrono::steady_clock::now();
    TuOutcome out;

    try
    {
        const bool isCuda = endsWithCu(entry.file);
        const cuda::CudaMode requestedMode = options.sanitizerOptions.cudaMode;

        static const cuda::CudaMode kRungs[] = {cuda::CudaMode::Full, cuda::CudaMode::NoWrapper, cuda::CudaMode::HostOnly};

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
                    "symhow: all CUDA rungs (full, nowrapper, host-only) failed to produce content for this translation unit",
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

        best.batch.stats.tuFile = symhow::common::paths::normalize(entry.file);
        best.batch.stats.parsedRungIndex = rungIndex;
        best.batch.stats.parsedOk = best.contentOk;
        best.batch.stats.parseMs = best.parseMs;
        best.batch.stats.walkMs = best.walkMs;
        best.batch.stats.diagErrors = best.diagErrors;
        best.batch.stats.diagFatal = best.diagFatal;

        out.hasContent = best.batch.hasContent();
        out.rungIndex = rungIndex;

        // Per-TU status persisted into symhow_tu.status (see Provenance.h) --
        // "failed" (no content), "partial" (content but a fatal diagnostic,
        // or a CUDA run that only succeeded on a degraded fallback rung),
        // "ok" otherwise. rungIndex is only meaningful for the CUDA
        // Auto-mode retry ladder (0 for non-CUDA / non-auto): 1 = Full
        // (not degraded), 2/3 = NoWrapper/HostOnly (degraded fallback).
        if (!out.hasContent)
        {
            out.status = "failed";
        }
        else if (best.batch.stats.diagFatal > 0 || rungIndex >= 2)
        {
            out.status = "partial";
        }
        else
        {
            out.status = "ok";
        }

        out.batch = std::move(best.batch);
    }
    catch (const std::exception& e)
    {
        // Defensive: every known throwing step above (sanitize(), the CUDA
        // ladder) already turns its own exceptions into a minimalFailureBatch
        // internally, but a worker thread must NEVER let an exception
        // escape and terminate the process (see Feature 2's thread-safety
        // requirements) -- so this is a last-resort net, not the normal path.
        out.batch = minimalFailureBatch(entry.file, std::string("symhow: unexpected exception during extraction: ") + e.what(), true);
        out.batch.stats.tuFile = symhow::common::paths::normalize(entry.file);
        out.hasContent = false;
        out.status = "failed";
        out.rungIndex = 0;
    }
    catch (...)
    {
        out.batch = minimalFailureBatch(entry.file, "symhow: unknown exception during extraction", true);
        out.batch.stats.tuFile = symhow::common::paths::normalize(entry.file);
        out.hasContent = false;
        out.status = "failed";
        out.rungIndex = 0;
    }

    out.extractSeconds = std::chrono::duration<double>(std::chrono::steady_clock::now() - entryStart).count();
    return out;
}

// The display status shown on a per-TU progress line ("ok" / "ok (cuda rung
// N)" / "no content" / "failed") -- a strictly display-only refinement of
// TuOutcome::hasContent/status: "failed" is shown when at least one FATAL
// ErrorFact was recorded (a hard parse/sanitize failure), "no content"
// otherwise (the TU parsed but produced no symbols/references, e.g. a
// pure-declaration header with nothing indexable). Never used for anything
// persisted -- TuOutcome::status (ok/partial/failed) is what lands in
// symhow_tu.status.
// Ranks symhow_tu.status for the worst-of-N-entries merge a multi-entry path
// needs (see PendingTuRow's doc comment): "failed" is worse than "partial"
// is worse than "ok". Unrecognized strings are never produced by
// processEntry() and rank as "ok" (0) defensively.
int statusRank(const std::string& status)
{
    if (status == "failed")
    {
        return 2;
    }
    if (status == "partial")
    {
        return 1;
    }
    return 0;
}

// Worse of `a` and `b` under statusRank() -- ties keep `a` (arbitrary but
// deterministic).
const std::string& worseStatus(const std::string& a, const std::string& b)
{
    return statusRank(b) > statusRank(a) ? b : a;
}

std::string displayStatusFor(const TuOutcome& outcome)
{
    std::string label;
    if (outcome.hasContent)
    {
        label = "ok";
    }
    else
    {
        bool fatalPresent = false;
        for (const auto& e : outcome.batch.errors)
        {
            if (e.fatal)
            {
                fatalPresent = true;
                break;
            }
        }
        label = fatalPresent ? "failed" : "no content";
    }
    if (outcome.rungIndex > 0)
    {
        label += " (cuda rung " + std::to_string(outcome.rungIndex) + ")";
    }
    return label;
}

// Formats and prints (to stderr, flushed) the per-TU progress line and
// periodic running-summary lines described in the progress-display feature.
// Only ever touched from the single consumer thread (the main thread in the
// sequential path, or runIndexParallel's consumer loop), strictly in CDB
// order -- never from a worker thread.
class ProgressReporter
{
public:
    ProgressReporter(uint32_t total, int quietLevel, std::string repoRoot)
        : m_total(total),
          m_quietLevel(quietLevel),
          m_repoRoot(std::move(repoRoot)),
          m_start(std::chrono::steady_clock::now()),
          m_lastSummary(m_start)
    {
    }

    void onTuDone(const std::string& absPath, double seconds, const TuOutcome& outcome)
    {
        ++m_done;
        if (m_quietLevel == 0)
        {
            const std::string path = symhow::index::progress::relativeToRoot(absPath, m_repoRoot);
            std::fprintf(
                stderr,
                "%s\n",
                symhow::index::progress::formatProgressLine(m_done, m_total, seconds, displayStatusFor(outcome), path).c_str());
            std::fflush(stderr);
        }
        maybeSummary(/*force=*/m_done >= m_total);
    }

    // Called once after the loop, in case the last onTuDone() call didn't
    // already emit the final periodic summary (e.g. tusTotal == 0).
    void finish() { maybeSummary(/*force=*/true); }

private:
    void maybeSummary(bool force)
    {
        if (m_quietLevel >= 2)
        {
            // -qq: no periodic summary at all -- only the unconditional
            // final "symhow index: N/N TUs produced content..." line (printed
            // separately, unconditionally, by runIndex()) survives.
            return;
        }

        // Nothing new to report since the last summary -- e.g. finish()
        // called after onTuDone() already force-printed at done==total, or
        // tusTotal==0 to begin with. Checked BEFORE the force bypass below
        // so finish() never re-prints an identical line onTuDone() already
        // emitted for the exact same `m_done`.
        if (m_done == m_lastSummaryDone)
        {
            return;
        }

        const auto now = std::chrono::steady_clock::now();
        const double sinceLast = std::chrono::duration<double>(now - m_lastSummary).count();
        const double intervalSeconds = (m_quietLevel == 1) ? 60.0 : 5.0;
        const bool countTrigger = (m_quietLevel == 0) && (m_done - m_lastSummaryDone) >= 10;
        const bool timeTrigger = sinceLast >= intervalSeconds;

        if (!force && !countTrigger && !timeTrigger)
        {
            return;
        }

        const double elapsedSeconds = std::chrono::duration<double>(now - m_start).count();
        const double elapsedMinutes = elapsedSeconds / 60.0;
        const double tusPerMin = elapsedMinutes > 0.0 ? static_cast<double>(m_done) / elapsedMinutes : 0.0;
        double etaSeconds = 0.0;
        if (tusPerMin > 0.0 && m_done < m_total)
        {
            const double remaining = static_cast<double>(m_total - m_done);
            etaSeconds = (remaining / tusPerMin) * 60.0;
        }

        std::fprintf(
            stderr,
            "%s\n",
            symhow::index::progress::formatRunningSummary(m_done, m_total, tusPerMin, elapsedSeconds, etaSeconds).c_str());
        std::fflush(stderr);

        m_lastSummary = now;
        m_lastSummaryDone = m_done;
    }

    uint32_t m_total;
    int m_quietLevel;
    std::string m_repoRoot;
    std::chrono::steady_clock::time_point m_start;
    std::chrono::steady_clock::time_point m_lastSummary;
    uint32_t m_done = 0;
    uint32_t m_lastSummaryDone = 0;
};

}  // namespace

int runIndex(const IndexRunOptions& options, symhow::model::IndexSummary* summaryOut)
{
    installInterruptHandlers();

    if (!options.appendToExisting && std::filesystem::exists(options.outputPath))
    {
        if (!options.force)
        {
            // An existing output left with symhow_index_in_progress="1" is an
            // interrupted `symhow index` run, not just "a database that happens
            // to be there" -- point at the actual recovery path (`symhow
            // update`) rather than only offering --force (which discards
            // all the already-completed work).
            bool inProgress = false;
            try
            {
                symhow::db::Database existing(options.outputPath);
                inProgress = symhow::db::getMeta(existing.handle(), "symhow_index_in_progress").value_or("") == "1";
            }
            catch (const std::exception&)
            {
                // Not a readable symhow-schema db (or no such meta row) --
                // treat as the ordinary "already exists" case below.
            }
            if (inProgress)
            {
                std::fprintf(
                    stderr,
                    "symhow index: output '%s' contains an interrupted index run -- finish it with `symhow update -d %s`, "
                    "or pass --force to discard it and start over\n",
                    options.outputPath.c_str(),
                    options.outputPath.c_str());
                return 2;
            }
            std::fprintf(
                stderr, "symhow index: output '%s' already exists (use --force to overwrite)\n", options.outputPath.c_str());
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
            std::fprintf(stderr, "symhow index: %s\n", e.what());
            return 1;
        }
    }

    if (options.limit >= 0 && static_cast<std::size_t>(options.limit) < entries.size())
    {
        entries.resize(static_cast<std::size_t>(options.limit));
    }

    // Real compile_commands.json files can list the same `file` path more
    // than once (e.g. compiled for several targets with different flags) --
    // symhow_tu is keyed by path, so every entry of a duplicated path must
    // agree on one combined, order-independent args_hash/entry_count
    // (symhow::index::pathArgsHashes(), see its doc comment) rather than
    // whichever entry happened to be consumed (and therefore UPSERTed)
    // last -- that used to make `symhow update` see a false "flag changed" on
    // EVERY run for any duplicated path. Computed once, up front, over the
    // final `entries` list (post limit/--stop-after doesn't apply here --
    // this covers the CDB as loaded, matching what `symhow update` will
    // independently recompute from the same CDB).
    const std::unordered_map<std::string, PathArgsInfo> pathHashes = pathArgsHashes(entries);

    SrctrldbBackend backend;
    backend.setNoVacuum(options.noVacuum);
    backend.setVerbose(options.verbose);

    std::string err;
    if (!backend.begin(options.outputPath, &err))
    {
        std::fprintf(stderr, "symhow index: failed to open output database '%s': %s\n", options.outputPath.c_str(), err.c_str());
        return 1;
    }

    // Provenance (+ symhow_index_in_progress="1") up front: written via its own
    // tiny WritableDb transaction, BEFORE the writer's first
    // beginTransaction() -- so it's durable even if this run is interrupted
    // before a single TU is consumed. See writeProvenanceEarly()'s comment.
    try
    {
        writeProvenanceEarly(options.outputPath, options.provenance);
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "symhow index: failed to write provenance: %s\n", e.what());
        return 1;
    }

    if (!backend.startTransaction(&err))
    {
        std::fprintf(stderr, "symhow index: failed to start writer transaction: %s\n", err.c_str());
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

    const int quietLevel = options.superQuiet ? 2 : (options.quiet ? 1 : 0);
    const std::string repoRoot = options.provenance.has_value() ? options.provenance->repoRoot : std::string();
    ProgressReporter reporter(tusTotal, quietLevel, repoRoot);

    // Checkpointing state (Phase 8): `chunkTuRows` accumulates one
    // PendingTuRow per TU consumed since the last checkpoint (cleared by
    // doCheckpoint() below); `sinceCheckpoint` is its size, tracked
    // separately for the commitEvery threshold check.
    std::vector<SrctrldbBackend::PendingTuRow> chunkTuRows;
    uint32_t sinceCheckpoint = 0;
    bool checkpointFailed = false;

    // Worst-so-far symhow_tu.status per path, across every entry of that path
    // consumed so far THIS session -- by the time a path's LAST entry (in
    // CDB order) is consumed, this is the worst status of all its entries
    // (see PendingTuRow's doc comment). Persists across checkpoints (unlike
    // chunkTuRows, which is only the since-last-checkpoint delta) -- a path
    // whose first entry is "ok" but whose second entry (in a LATER
    // checkpoint) is "failed" must still end up "failed".
    std::unordered_map<std::string, std::string> worstStatusByPath;

    // Entries of each path still to be consumed THIS session, initialized
    // from pathHashes' entryCount and decremented by consumeOutcome() below
    // -- the `symhow_tu` row for a path is only ever UPSERTed once its count
    // hits 0, i.e. once its LAST entry (in CDB order -- consumeOutcome is
    // called strictly in that order, both in the sequential path and via
    // the parallel reorder buffer) has actually been consumed. This is
    // deliberate, not an optimization: writing the row as soon as ANY entry
    // of a multi-entry path was consumed (the earlier design) meant an
    // interrupted run that stopped strictly BETWEEN two entries of the same
    // duplicated path left a `symhow_tu` row behind that claimed the path was
    // fully accounted for -- `symhow update`'s Phase 8 resume logic keys off
    // "no `symhow_tu` row at all" to find never-indexed CDB entries, so it would
    // silently skip re-indexing the still-unconsumed sibling entry forever.
    // Gating the UPSERT on "last entry consumed" makes an interrupted,
    // split-duplicated-path run leave NO row for that path at all -- resume
    // then correctly re-indexes ALL of its entries (selectEntries() already
    // selects whole paths, not individual entries -- see FileMatcher.cpp).
    // Re-processing the already-checkpointed first entry a second time on
    // resume is harmless: SrctrldbBackend's node/symbol dedup (`m_byKey` et
    // al.) is idempotent for identical facts. See docs/KNOWN_LIMITATIONS.md's
    // "Phase 9" section.
    std::unordered_map<std::string, uint32_t> remainingEntriesByPath;
    remainingEntriesByPath.reserve(pathHashes.size());
    for (const auto& [path, info] : pathHashes)
    {
        remainingEntriesByPath[path] = info.entryCount;
    }

    auto doCheckpoint = [&]() {
        if (checkpointFailed || chunkTuRows.empty())
        {
            return;
        }
        std::string cpErr;
        if (!backend.checkpoint(chunkTuRows, &cpErr))
        {
            std::fprintf(stderr, "symhow index: checkpoint failed: %s\n", cpErr.c_str());
            checkpointFailed = true;
        }
        chunkTuRows.clear();
        sinceCheckpoint = 0;
    };

    // Consumes one TU's already-produced outcome: updates the running
    // counts, feeds the batch to the single SrctrldbBackend writer, buffers
    // its symhow_tu row for the next checkpoint, and reports progress.
    // Deliberately the ONLY place that touches `backend`/the counters/
    // `reporter` -- called exactly once per entry, strictly in CDB order,
    // from a single thread (the main thread in the sequential path;
    // runIndexParallel's consumer loop otherwise).
    auto consumeOutcome = [&](const CompileEntry& entry, TuOutcome& outcome) {
        ++tusAttempted;
        if (outcome.hasContent)
        {
            ++tusWithContent;
        }
        else
        {
            ++tusFailed;
            failedTus.push_back(entry.file);
        }

        const auto consumeStart = std::chrono::steady_clock::now();
        std::string consumeErr;
        if (!backend.consume(outcome.batch, &consumeErr))
        {
            std::fprintf(stderr, "symhow index: failed to write facts for '%s': %s\n", entry.file.c_str(), consumeErr.c_str());
        }
        const double consumeSeconds = std::chrono::duration<double>(std::chrono::steady_clock::now() - consumeStart).count();

        // Combined, order-independent per-path hash/entry_count (see
        // pathArgsHashes()'s doc comment) -- not computeArgsHash(entry), so
        // whichever entry ends up UPSERTing `symhow_tu` writes the SAME
        // args_hash regardless of which of the path's entries that is.
        // status is the worst seen so far for this path, across every one
        // of its entries consumed to date. The row itself is only pushed
        // into chunkTuRows once THIS path's last entry is consumed (see
        // remainingEntriesByPath's doc comment above) -- an interrupted run
        // that stops mid-path leaves no row for it at all.
        const PathArgsInfo& pathInfo = pathHashes.at(entry.file);
        std::string& worst = worstStatusByPath[entry.file];
        worst = worst.empty() ? outcome.status : worseStatus(worst, outcome.status);
        const uint32_t remaining = --remainingEntriesByPath.at(entry.file);
        if (remaining == 0)
        {
            chunkTuRows.push_back(SrctrldbBackend::PendingTuRow{entry.file, pathInfo.combinedHash, worst, pathInfo.entryCount});
        }
        ++sinceCheckpoint;
        if (options.commitEvery > 0 && sinceCheckpoint >= static_cast<uint32_t>(options.commitEvery))
        {
            doCheckpoint();
        }

        reporter.onTuDone(outcome.batch.stats.tuFile, outcome.extractSeconds + consumeSeconds, outcome);
    };

    // True once either a real SIGINT/SIGTERM was observed, or
    // --stop-after's TU count was reached -- checked once per consumed TU,
    // from the single consumer thread only.
    auto interruptRequested = [&]() {
        return g_interruptRequested != 0 || (options.stopAfter >= 0 && tusAttempted >= static_cast<uint32_t>(options.stopAfter));
    };
    bool interrupted = false;

    const unsigned hwThreads = std::max(1u, std::thread::hardware_concurrency());
    const unsigned numThreads = options.parallel == 0 ? hwThreads : static_cast<unsigned>(std::max(1, options.parallel));

    if (numThreads <= 1 || entries.size() <= 1)
    {
        // Sequential path: unchanged in spirit from the pre-parallel
        // implementation (and byte-for-byte identical in the facts it
        // produces) -- process each TU, then immediately consume it. Checked
        // after each consume(): with no worker threads in flight, this gives
        // an EXACT interruption point (exactly K TUs consumed for
        // --stop-after K).
        for (const auto& entry : entries)
        {
            TuOutcome outcome = processEntry(entry, options, fileRegOptions);
            consumeOutcome(entry, outcome);
            if (checkpointFailed)
            {
                break;
            }
            if (interruptRequested())
            {
                interrupted = true;
                break;
            }
        }
    }
    else
    {
        // Parallel path: a fixed pool of `numThreads` workers pulls entry
        // indices from `nextDispatch` (in CDB order) and runs the full,
        // self-contained processEntry() extraction for each. The MAIN
        // thread is the sole consumer: it drains results[0], results[1],
        // ... strictly in order (a "reorder buffer"), so every observable
        // side effect of indexing -- node ids, symhow_tu rows, collision-merge
        // outcomes, progress lines -- is byte-for-byte identical to the
        // sequential path above, regardless of `numThreads`. Workers are
        // blocked from racing more than `2*numThreads` entries ahead of the
        // consumer (bounded memory: at most that many in-flight/finished-
        // but-unconsumed TuOutcomes are alive at once).
        const std::size_t numEntries = entries.size();
        const std::size_t windowCap = 2 * static_cast<std::size_t>(numThreads);

        std::vector<TuOutcome> results(numEntries);
        std::vector<char> ready(numEntries, 0);

        std::mutex mtx;
        std::condition_variable cv;
        std::size_t nextDispatch = 0;
        std::size_t nextConsume = 0;

        auto workerLoop = [&]() {
            while (true)
            {
                std::size_t idx;
                {
                    std::unique_lock<std::mutex> lock(mtx);
                    cv.wait(lock, [&] { return nextDispatch >= numEntries || (nextDispatch - nextConsume) < windowCap; });
                    if (nextDispatch >= numEntries)
                    {
                        return;
                    }
                    idx = nextDispatch++;
                }

                TuOutcome outcome = processEntry(entries[idx], options, fileRegOptions);

                {
                    std::lock_guard<std::mutex> lock(mtx);
                    results[idx] = std::move(outcome);
                    ready[idx] = 1;
                }
                cv.notify_all();
            }
        };

        std::vector<std::thread> workers;
        workers.reserve(numThreads);
        for (unsigned t = 0; t < numThreads; ++t)
        {
            workers.emplace_back(workerLoop);
        }

        // `dispatchCutoff`: once interrupted, no index >= this value will
        // EVER become ready (dispatch was halted right after snapshotting
        // it) -- the wait predicate below must stop waiting for those
        // instead of blocking forever. Starts at numEntries (no cutoff).
        std::size_t dispatchCutoff = numEntries;

        for (std::size_t i = 0; i < numEntries; ++i)
        {
            if (interrupted && i >= dispatchCutoff)
            {
                break;
            }
            TuOutcome outcome;
            {
                std::unique_lock<std::mutex> lock(mtx);
                // NOTE: must wait on `dispatchCutoff`, NOT the live
                // `nextDispatch` -- nextDispatch starts at 0 and legitimately
                // climbs from 0 as workers dispatch, so "nextDispatch <= i"
                // would be spuriously true for i=0 before any worker has
                // even started, breaking the loop before a single TU is ever
                // consumed. dispatchCutoff starts at numEntries (never
                // triggers) and is only ever lowered, deliberately, at the
                // moment of interrupt.
                cv.wait(lock, [&] { return ready[i] != 0 || i >= dispatchCutoff; });
                if (ready[i] == 0)
                {
                    // Dispatch was halted before this index was ever handed
                    // to a worker -- it will never become ready.
                    break;
                }
                outcome = std::move(results[i]);
            }
            consumeOutcome(entries[i], outcome);
            {
                std::lock_guard<std::mutex> lock(mtx);
                nextConsume = i + 1;
            }
            cv.notify_all();

            if (!interrupted && (checkpointFailed || interruptRequested()))
            {
                interrupted = !checkpointFailed;  // a checkpoint failure is a hard error, not a clean interrupt
                std::lock_guard<std::mutex> lock(mtx);
                dispatchCutoff = nextDispatch;  // freeze: only these were ever dispatched
                nextDispatch = numEntries;      // stop workers from taking new work
                cv.notify_all();
                if (checkpointFailed)
                {
                    break;
                }
            }
        }

        for (auto& w : workers)
        {
            w.join();
        }
    }

    reporter.finish();

    if (checkpointFailed)
    {
        std::fprintf(stderr, "symhow index: aborting after checkpoint failure\n");
        return 1;
    }

    if (interrupted)
    {
        std::string interruptErr;
        if (!backend.finishInterrupted(chunkTuRows, &interruptErr))
        {
            std::fprintf(stderr, "symhow index: failed to checkpoint on interrupt: %s\n", interruptErr.c_str());
            return 1;
        }
        std::fprintf(
            stderr,
            "symhow index: interrupted after %u/%u TUs -- completed work is saved; resume with: symhow update -d %s\n",
            tusAttempted,
            tusTotal,
            options.outputPath.c_str());
        return 130;
    }

    // Final checkpoint: covers whatever TUs weren't checkpointed yet -- for
    // commitEvery=0 this is the ONLY checkpoint() call, covering the entire
    // run (see SrctrldbBackend::finish()'s comment on why that makes the
    // result byte-for-byte equivalent to the pre-Phase-8 single-flush
    // behavior).
    {
        std::string cpErr;
        if (!backend.checkpoint(chunkTuRows, &cpErr))
        {
            std::fprintf(stderr, "symhow index: final checkpoint failed: %s\n", cpErr.c_str());
            return 1;
        }
        chunkTuRows.clear();
    }

    symhow::model::IndexSummary summary;
    if (!backend.finish(summary, &err))
    {
        std::fprintf(stderr, "symhow index: failed to finalize output database: %s\n", err.c_str());
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
        "symhow index: %u/%u TUs produced content (%u failed), %u symbols, %u references, %u files, %u errors, %u name "
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
        // symhow.index-summary/1 -- see docs/JSON_SCHEMA.md. Reshapes the same
        // IndexSummary data Phase 3 already printed (just as a flat,
        // ad-hoc-keyed object) into the Phase 5 envelope convention shared
        // by every other `symhow`/`symhow query` JSON schema (symhow_version + schema +
        // ...); the underlying indexing logic above is untouched.
        nlohmann::json j;
        j["symhow_version"] = options.symhowVersion;
        j["schema"] = "symhow.index-summary/1";
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

}  // namespace symhow::index
