#include <catch2/catch_test_macros.hpp>

#include "kg/index/CudaSupport.h"

using namespace kg::index::cuda;

namespace
{

CudaSynthesisInput representativeInput()
{
    CudaSynthesisInput input;
    input.maxArch = 90;
    input.cudaPath = "/opt/cuda-toolkit";
    return input;
}

}  // namespace

TEST_CASE("cuda_support:parseCudaMode round-trips all valid spellings", "[cuda]")
{
    CHECK(parseCudaMode("auto") == CudaMode::Auto);
    CHECK(parseCudaMode("full") == CudaMode::Full);
    CHECK(parseCudaMode("nowrapper") == CudaMode::NoWrapper);
    CHECK(parseCudaMode("host-only") == CudaMode::HostOnly);
    CHECK(parseCudaMode("skip") == CudaMode::Skip);
    CHECK_THROWS_AS(parseCudaMode("bogus"), std::invalid_argument);
}

TEST_CASE("cuda_support:resolveAutoMode resolves Auto to Full for Phase 2", "[cuda]")
{
    CHECK(resolveAutoMode(CudaMode::Auto) == CudaMode::Full);
    CHECK(resolveAutoMode(CudaMode::Full) == CudaMode::Full);
    CHECK(resolveAutoMode(CudaMode::NoWrapper) == CudaMode::NoWrapper);
    CHECK(resolveAutoMode(CudaMode::HostOnly) == CudaMode::HostOnly);
    CHECK(resolveAutoMode(CudaMode::Skip) == CudaMode::Skip);
}

TEST_CASE("cuda_support:synthesize: Full rung emits -x cuda, --cuda-path, --cuda-gpu-arch, --cuda-host-only", "[cuda]")
{
    const auto flags = synthesize(CudaMode::Full, representativeInput());
    REQUIRE(
        flags ==
        std::vector<std::string>{
            "-x", "cuda", "--cuda-path=/opt/cuda-toolkit", "--cuda-gpu-arch=sm_90", "--cuda-host-only"});
}

TEST_CASE("cuda_support:synthesize: Full rung omits --cuda-path when unknown", "[cuda]")
{
    CudaSynthesisInput input = representativeInput();
    input.cudaPath.clear();
    const auto flags = synthesize(CudaMode::Full, input);
    REQUIRE(flags == std::vector<std::string>{"-x", "cuda", "--cuda-gpu-arch=sm_90", "--cuda-host-only"});
}

TEST_CASE("cuda_support:synthesize: NoWrapper rung emits -x cuda -nocudainc -nocudalib --cuda-host-only only", "[cuda]")
{
    const auto flags = synthesize(CudaMode::NoWrapper, representativeInput());
    REQUIRE(flags == std::vector<std::string>{"-x", "cuda", "-nocudainc", "-nocudalib", "--cuda-host-only"});
}

TEST_CASE("cuda_support:synthesize: HostOnly rung emits only -x c++", "[cuda]")
{
    const auto flags = synthesize(CudaMode::HostOnly, representativeInput());
    REQUIRE(flags == std::vector<std::string>{"-x", "c++"});
}

TEST_CASE("cuda_support:synthesize: Skip rung emits no flags at all", "[cuda]")
{
    const auto flags = synthesize(CudaMode::Skip, representativeInput());
    REQUIRE(flags.empty());
}

TEST_CASE("cuda_support:synthesize: Auto defers to Full (Phase 2 behavior)", "[cuda]")
{
    const auto autoFlags = synthesize(CudaMode::Auto, representativeInput());
    const auto fullFlags = synthesize(CudaMode::Full, representativeInput());
    REQUIRE(autoFlags == fullFlags);
}
