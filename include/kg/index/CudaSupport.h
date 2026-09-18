#pragma once

#include <string>
#include <vector>

// CUDA-flag synthesis (step 4 of the argument sanitizing pipeline), kept as
// its own module so it's independently callable/testable, separate from the
// nvcc-flag rewriting in ArgSanitizer (step 3).
namespace kg::index::cuda
{

// One "rung" of CUDA-handling strategy that clang can be asked to try when
// parsing a .cu file. Phase 3 will retry a .cu TU across these rungs
// (Full -> NoWrapper -> HostOnly -> Skip) until one succeeds; Phase 2 only
// ever resolves Auto to Full (see resolveAutoMode()) since there is no real
// parse-and-retry loop yet to make that decision meaningfully.
enum class CudaMode
{
    Auto,
    Full,
    NoWrapper,
    HostOnly,
    Skip,
};

// Parses a --cuda-mode CLI value ("auto", "full", "nowrapper", "host-only",
// "skip"). Throws std::invalid_argument on anything else.
CudaMode parseCudaMode(const std::string& value);

// Returns a human-readable name for `mode` (the inverse of parseCudaMode,
// used for diagnostics).
std::string cudaModeName(CudaMode mode);

// Resolves CudaMode::Auto to a concrete mode.
//
// Phase 2 note: there is no real libclang parse-and-retry logic yet (that
// lands in Phase 3), so for --dry-run purposes Auto simply resolves to
// Full. Phase 3 will replace this with real multi-rung retry logic that
// calls synthesize() once per rung (Full, then NoWrapper, then HostOnly,
// then Skip) until a rung's argv actually parses successfully.
CudaMode resolveAutoMode(CudaMode mode);

// Inputs to synthesize() that vary per translation unit / per CDB entry.
struct CudaSynthesisInput
{
    // Highest compute_N / sm_N number seen across the entry's
    // --generate-code/-gencode/--gpu-architecture flags (ArgSanitizer's
    // step-3 rewrite pass computes this while dropping those flags).
    // Defaults to 90 if the entry had none at all.
    int maxArch = 90;

    // Resolved CUDA toolkit root (the directory containing bin/nvcc,
    // include/, etc.), or empty if it couldn't be resolved (--cuda-path,
    // $KG_CUDA_PATH, and derivation from the nvcc executable path all
    // failed/were unavailable). When empty, Full rung omits --cuda-path
    // entirely and lets clang's own auto-detection try (which may fail --
    // that's an accepted, documented fallback path).
    std::string cudaPath;
};

// Step 4: returns the CUDA-related flags to append to a .cu TU's argv for
// the given (already-resolved, non-Auto) rung. Does NOT include any of the
// step-3-rewritten or step-5-injected flags -- just this step's additions.
//
//   Full:      -x cuda --cuda-path=<path> (if known) --cuda-gpu-arch=sm_<maxArch> --cuda-host-only
//   NoWrapper: -x cuda -nocudainc -nocudalib --cuda-host-only
//              (no --cuda-path / --cuda-gpu-arch: verified separately that
//              plain -nocudainc -nocudalib --cuda-host-only suffices)
//   HostOnly:  -x c++
//              (intentionally loses <<<>>> kernel-launch syntax -- documented
//              behavior, see docs/KNOWN_LIMITATIONS.md)
//   Skip:      {} (caller is responsible for not parsing the file at all)
//   Auto:      resolved via resolveAutoMode() and re-dispatched (defensive;
//              callers are expected to resolve Auto themselves first)
std::vector<std::string> synthesize(CudaMode mode, const CudaSynthesisInput& input);

}  // namespace kg::index::cuda
