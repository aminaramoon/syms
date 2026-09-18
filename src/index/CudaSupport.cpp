#include "kg/index/CudaSupport.h"

#include <stdexcept>

namespace kg::index::cuda
{

CudaMode parseCudaMode(const std::string& value)
{
    if (value == "auto")
    {
        return CudaMode::Auto;
    }
    if (value == "full")
    {
        return CudaMode::Full;
    }
    if (value == "nowrapper")
    {
        return CudaMode::NoWrapper;
    }
    if (value == "host-only")
    {
        return CudaMode::HostOnly;
    }
    if (value == "skip")
    {
        return CudaMode::Skip;
    }
    throw std::invalid_argument(
        "invalid --cuda-mode value '" + value + "' (expected auto, full, nowrapper, host-only, or skip)");
}

std::string cudaModeName(CudaMode mode)
{
    switch (mode)
    {
        case CudaMode::Auto:
            return "auto";
        case CudaMode::Full:
            return "full";
        case CudaMode::NoWrapper:
            return "nowrapper";
        case CudaMode::HostOnly:
            return "host-only";
        case CudaMode::Skip:
            return "skip";
    }
    return "unknown";
}

CudaMode resolveAutoMode(CudaMode mode)
{
    // See the Phase-2 note on CudaMode/resolveAutoMode in CudaSupport.h:
    // Auto -> Full is a deliberate Phase 2 simplification, not a real
    // rung-selection algorithm.
    if (mode == CudaMode::Auto)
    {
        return CudaMode::Full;
    }
    return mode;
}

std::vector<std::string> synthesize(CudaMode mode, const CudaSynthesisInput& input)
{
    switch (mode)
    {
        case CudaMode::Auto:
            return synthesize(resolveAutoMode(mode), input);

        case CudaMode::Full:
        {
            std::vector<std::string> flags = {"-x", "cuda"};
            if (!input.cudaPath.empty())
            {
                flags.push_back("--cuda-path=" + input.cudaPath);
            }
            flags.push_back("--cuda-gpu-arch=sm_" + std::to_string(input.maxArch));
            flags.push_back("--cuda-host-only");
            return flags;
        }

        case CudaMode::NoWrapper:
            return {"-x", "cuda", "-nocudainc", "-nocudalib", "--cuda-host-only"};

        case CudaMode::HostOnly:
            return {"-x", "c++"};

        case CudaMode::Skip:
            return {};
    }
    return {};
}

}  // namespace kg::index::cuda
