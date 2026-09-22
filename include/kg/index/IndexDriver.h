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
    bool jsonSummary = false;
    bool noVacuum = false;
    bool verbose = false;

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

    // When set, written to `meta` (via kg::db::WritableDb) after finish()
    // succeeds, together with one kg_tu row per indexed CDB entry.
    std::optional<Provenance> provenance;
};

// Runs the full pipeline. Returns the process exit code: 0 if at least one
// TU produced content, 5 if none did, 1 on a hard error (e.g. can't open
// the compile database, output path exists without --force, can't open the
// output database). If `summaryOut` is non-null, the final IndexSummary is
// also copied there (the CLI itself passes nullptr; it only needs the
// stderr/--json reporting runIndex() already does).
int runIndex(const IndexRunOptions& options, kg::model::IndexSummary* summaryOut = nullptr);

}  // namespace kg::index
