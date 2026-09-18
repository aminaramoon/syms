#pragma once

#include <string>

#include "kg/index/ArgSanitizer.h"
#include "kg/index/CompileDb.h"
#include "kg/index/FileRegistry.h"

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
};

// Runs the full pipeline. Returns the process exit code: 0 if at least one
// TU produced content, 5 if none did, 1 on a hard error (e.g. can't open
// the compile database, output path exists without --force, can't open the
// output database).
int runIndex(const IndexRunOptions& options);

}  // namespace kg::index
