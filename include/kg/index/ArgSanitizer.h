#pragma once

#include <string>
#include <vector>

#include "kg/index/CompileDb.h"
#include "kg/index/CudaSupport.h"

// Argument sanitizing pipeline (steps 2, 3, 5 of the design; step 4 is
// delegated to kg::index::cuda::synthesize(), see CudaSupport.h). Step 1
// (REMAP) happens earlier, in kg::index::CompileDb::load(); by the time a
// CompileEntry reaches sanitize(), its directory/file/args are assumed to
// already be remapped.
namespace kg::index
{

// Options controlling sanitize(), populated from `kg index` CLI flags (see
// kg::cli). A couple of fields document a subtlety in how their
// env-var/default fallback chain is split between the CLI layer and this
// module:
//
//   - resourceDir: whatever this field holds is emitted verbatim as
//     `-resource-dir=<value>` -- it MUST be what clang's own -resource-dir
//     flag expects (the parent of the include/ directory holding builtin
//     headers like stddef.h, i.e. exactly what `clang -print-resource-dir`
//     reports), not the include/ directory itself; see the "Resource dir
//     discovery" comment in cmake/FindLibClang.cmake for the bug this
//     distinction fixed in Phase 3. The CLI layer (Cli.cpp, part of the
//     `kg` executable target) resolves --resource-dir and then
//     $KG_RESOURCE_DIR into this field, leaving it empty if neither was
//     set. sanitize() itself then falls back to the compile-time
//     KG_DEFAULT_RESOURCE_DIR macro *inside ArgSanitizer.cpp* -- that macro
//     is a PRIVATE compile definition of the kg_index CMake target (set in
//     src/CMakeLists.txt), so it is not visible from Cli.cpp at all; only
//     translation units that are part of kg_index (like this one) can see
//     it.
//   - cudaPath: the CLI layer resolves --cuda-path and then $KG_CUDA_PATH
//     into this field. If still empty and the entry's argv[0] turned out
//     to be nvcc, sanitize() derives a path from that nvcc executable's
//     location (dirname(dirname(argv[0])), since nvcc lives at
//     <toolkit>/bin/nvcc). If that's also unavailable, --cuda-path is
//     simply omitted from Full-rung synthesis.
struct SanitizerOptions
{
    std::vector<std::string> dropArgPatterns;              // --drop-arg glob patterns (step 3c)
    std::vector<std::string> extraArgs;                    // --extra-arg tokens, appended last (step 5)
    std::string resourceDir;                                // resolved --resource-dir / $KG_RESOURCE_DIR, or empty
    std::string gccToolchain;                                // --gcc-toolchain value, or empty = omit
    std::string stdFallback = "c++17";                       // --std value or default; used only if no -std=* survives
    std::string cudaPath;                                    // resolved --cuda-path / $KG_CUDA_PATH, or empty
    cuda::CudaMode cudaMode = cuda::CudaMode::Auto;           // --cuda-mode
};

// Result of sanitizing one CompileEntry.
struct SanitizeResult
{
    std::string basename;                    // argv[0] basename (recorded in step 2), for diagnostics
    bool isNvcc = false;
    bool isHostCxx = false;
    bool skipped = false;                    // true if CudaMode::Skip applies to this .cu file
    cuda::CudaMode cudaModeUsed = cuda::CudaMode::Auto;  // only meaningful if this entry went through CUDA synthesis
    std::vector<std::string> args;           // fully-sanitized argv, EXCLUDING argv[0] (libclang supplies its own)
};

// Runs steps 2 (split argv0), 3 (rewrite pass: nvcc rules, universal drop
// list, user --drop-arg patterns), 4 (CUDA synthesis, only for .cu files or
// when argv0 is nvcc), and 5 (injections: working-directory, resource-dir,
// -fsyntax-only, -w, -ferror-limit=0, -std fallback, --gcc-toolchain,
// --extra-arg) against a single (already step-1-remapped) CompileEntry.
SanitizeResult sanitize(const CompileEntry& entry, const SanitizerOptions& options);

}  // namespace kg::index
