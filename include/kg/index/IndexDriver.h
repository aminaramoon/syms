#pragma once

#include <optional>
#include <string>
#include <vector>

#include "kg/index/ArgSanitizer.h"
#include "kg/index/CompileDb.h"
#include "kg/index/FileRegistry.h"
#include "kg/index/Provenance.h"
#include "kg/model/IndexBackend.h"

// Orchestrates a full (non-dry-run) `kg index` run: load the compile
// database, then for each entry sanitize -> parse -> walk -> hand the
// resulting TuFactBatch to a SrctrldbBackend, sequentially. This is where
// the real (not Phase-2-stub) CUDA rung retry loop lives.
namespace kg::index
{

struct IndexRunOptions
{
    std::string compileCommandsPath;
    std::string outputPath = "./kg.srctrldb";
    bool force = false;
    long limit = -1;  // -1 = no limit

    bool quiet = false;
    // Set when the user passed -q/--quiet twice (kg CLI); suppresses even
    // the periodic (every-60s) summary line that a single -q keeps.
    bool superQuiet = false;
    bool jsonSummary = false;
    bool noVacuum = false;
    bool verbose = false;

    // --parallel/-j N: number of worker threads used to extract TUs
    // (sanitize -> parse -> walk). 1 (default) = fully sequential, byte-
    // for-byte identical to the pre-parallel implementation. 0 =
    // std::thread::hardware_concurrency(). N > 1 runs a fixed pool of N
    // worker threads, but the single SrctrldbBackend writer is still fed
    // batches strictly in CDB order (see IndexDriver.cpp's reorder buffer),
    // so the RESULT is deterministic and identical regardless of N -- only
    // wall-clock time changes. Not recorded in provenance: it doesn't
    // affect what gets indexed, only how fast.
    int parallel = 1;

    // Only used when jsonSummary is set: the "kg_version" field of the
    // kg.index-summary/1 envelope (KG_VERSION_STRING) -- passed in from the
    // CLI target rather than baked in here, since kg_index itself has no
    // compile-time coupling to that macro (only the `kg` executable target
    // defines it).
    std::string kgVersion;

    CompileDbOptions dbOptions;          // remap + include/exclude globs
    SanitizerOptions sanitizerOptions;   // cudaMode here is the user's requested default (Auto unless overridden)
    SystemFileRows systemFileRows = SystemFileRows::Direct;

    // Phase 1 incremental-update groundwork (kg update itself lands in
    // Phase 2; these fields exist now so the write path/provenance plumbing
    // is exercised by every `kg index` run).

    // When true, skip the exists/--force delete guard entirely: the output
    // path is opened and appended to (or created fresh if it doesn't exist
    // yet) rather than ever being deleted. `force` is ignored in this mode.
    bool appendToExisting = false;

    // When set, skip load() entirely and index exactly these entries
    // (already remapped/filtered by the caller) instead of re-parsing
    // compileCommandsPath from scratch.
    std::optional<std::vector<CompileEntry>> presetEntries;

    // When set, written to `meta` (via kg::db::WritableDb) right after
    // begin() succeeds -- i.e. BEFORE any TU is consumed -- together with
    // `kg_index_in_progress="1"`. kg_tu rows are written incrementally, one
    // checkpoint's worth at a time (see commitEvery below), not just at the
    // end.
    std::optional<Provenance> provenance;

    // Crash-safe/resumable indexing (checkpoints): after every `commitEvery`
    // consumed TUs -- and once more at the end -- the consumer thread
    // checkpoints: commits the writer's current transaction, flushes every
    // buffered node.type/definition_kind/kg_symbol_def/kg_touched/kg_tu
    // write via a second WritableDb connection in its own transaction, and
    // reopens a new writer transaction. 0 disables checkpointing entirely
    // (one single transaction for the whole run -- the pre-Phase-8
    // behavior). Default 25. Does not affect the RESULT (see
    // SrctrldbBackend::checkpoint()'s doc comment for why), only how much
    // work an interrupted run loses.
    int commitEvery = 25;

    // Hidden testing hook: if >= 0, index exactly this many TUs and then
    // behave exactly as if SIGINT/SIGTERM had just been delivered (final
    // checkpoint, print the "interrupted" message, exit 130) -- lets tests
    // exercise the interrupt/resume path deterministically, without signals.
    long stopAfter = -1;
};

// Runs the full pipeline. Returns the process exit code: 0 if at least one
// TU produced content, 5 if none did, 1 on a hard error (e.g. can't open
// the compile database, can't open the output database), 2 if the output
// path exists without --force (helpfully distinguishing "an interrupted
// index is sitting there -- run `kg update` or pass --force"), 130 if
// interrupted by SIGINT/SIGTERM (or --stop-after) -- completed work (up to
// the last checkpoint) is saved, and `kg update -d <output>` resumes. If
// `summaryOut` is non-null, the final IndexSummary is also copied there (the
// CLI itself passes nullptr; it only needs the stderr/--json reporting
// runIndex() already does).
int runIndex(const IndexRunOptions& options, kg::model::IndexSummary* summaryOut = nullptr);

}  // namespace kg::index
