#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <vector>

#include "kg/index/ArgSanitizer.h"
#include "kg/index/CompileDb.h"
#include "kg/index/FileRegistry.h"
#include "kg/update/Invalidator.h"

// Phase 3: `kg update` orchestration -- reads a database's stored
// provenance, works out which files changed since the last index/update
// (via git or an explicit file list), clears the affected facts
// (kg::update::Invalidator) and re-indexes only what's necessary
// (kg::index::runIndex with presetEntries), all against a temp copy that's
// atomically renamed over the original on success (or, with --in-place,
// directly against the real file with a recovery flag set for the
// duration).
namespace kg::update
{

struct UpdateRunOptions
{
    std::string dbPath;
    std::string repoRoot;  // --repo override; empty = use stored kg_repo_root
    std::string fromRev;   // hidden --from override; empty = use stored kg_repo_commit
    std::string toRev;     // --to override; empty = "HEAD"

    std::string cdbPath;  // -c override; empty = use stored kg_cdb_path
    std::vector<std::string> explicitFiles;  // --files: bypass git entirely

    // Overrides for the stored options (kg_options_json). A field is only
    // treated as "given" if the corresponding *Given flag is set -- an
    // empty vector/string CLI override (e.g. --include given zero times)
    // must not be confused with "not given at all".
    kg::index::CompileDbOptions dbOverrides;
    bool dbOverridesGiven = false;  // true if --remap/--include/--exclude given at all
    bool excludeSubmodules = false;  // append `<submodule>/**` exclude globs from the repo's .gitmodules
    kg::index::cuda::CudaMode cudaModeOverride = kg::index::cuda::CudaMode::Auto;
    bool cudaModeGiven = false;
    kg::index::SystemFileRows systemFileRowsOverride = kg::index::SystemFileRows::Direct;
    bool systemFileRowsGiven = false;
    std::vector<std::string> extraArgOverride;
    bool extraArgGiven = false;
    std::vector<std::string> dropArgOverride;
    bool dropArgGiven = false;
    std::string stdOverride;
    bool stdGiven = false;
    std::string cStdOverride;
    bool cStdGiven = false;

    bool dryRun = false;
    bool inPlace = false;
    bool noVacuum = false;
    bool json = false;
    int verbose = 0;
    std::string kgVersion;

    // --parallel/-j N for the re-index step (kg::index::runIndex's
    // IndexRunOptions::parallel) -- see its doc comment. Default 1
    // (sequential, unchanged behavior). Deliberately NOT part of
    // kg_options_json/Provenance: it doesn't affect what gets indexed.
    int parallel = 1;
};

struct UpdateSummary
{
    std::string database;
    bool dryRun = false;
    bool inPlace = false;

    std::string repoRoot;
    std::string fromRev;
    std::string toRev;

    uint32_t changedCount = 0;
    uint32_t deletedCount = 0;
    uint32_t renamedCount = 0;
    uint32_t flagChangedCount = 0;
    uint32_t unmatchedCount = 0;
    std::vector<std::string> unmatchedPaths;

    ClearPlan plan;  // clearedPaths/clearedFileIds/seedFiles/includeClosureAdded/headersPruned

    uint32_t tusToIndex = 0;
    uint32_t tusNew = 0;
    uint32_t tusRetried = 0;
    uint32_t skippedMissing = 0;

    GcStats gc;

    // Populated (if indexing ran) from kg::model::IndexSummary, via
    // IndexRunOptions::jsonSummary=false + a direct summaryOut pointer --
    // mirrors kg.index-summary/1's "counts" object.
    uint32_t idxTusTotal = 0, idxTusAttempted = 0, idxTusWithContent = 0, idxTusFailed = 0;
    uint32_t idxSymbols = 0, idxReferences = 0, idxFiles = 0, idxErrors = 0, idxNameCollisions = 0;
    bool indexRan = false;

    uint64_t elapsedMs = 0;
    uint64_t indexElapsedMs = 0;

    bool nothingToDo = false;

    // Phase 3 of docs/NOTES_DESIGN.md: post-update `kg::notes::syncNotes()`
    // results, populated (notesRan = true) only when a notes dir was known
    // (meta.kg_notes_dir, or <repo root>/notes if it exists) AND actually
    // exists on disk. Never fails the update: a notes-sync exception is
    // caught and reported (notesRan stays false), never surfaced as a
    // non-zero exit code. `notesNewlyStale` counts serialized_names that
    // were NOT stale in `kg_note` before this update's re-sync but ARE
    // after it (comparing against whatever the previous `kg notes sync`
    // left behind -- the kg_note* cache is untouched by re-indexing itself).
    bool notesRan = false;
    uint32_t notesNoted = 0;
    uint32_t notesStale = 0;
    uint32_t notesNewlyStale = 0;
    uint32_t notesProblems = 0;
};

// Exit codes: 0 ok (including --dry-run / nothing-to-do), 1 hard error,
// 2 refused (legacy/no-provenance db, update already in progress, bad
// options), 5 nothing to do.
int runUpdate(const UpdateRunOptions& options, UpdateSummary* out = nullptr);

}  // namespace kg::update
