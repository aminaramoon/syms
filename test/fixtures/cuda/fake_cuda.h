// fake_cuda.h -- minimal, hand-written CUDA runtime stub for hermetic,
// toolkit-free indexing (Phase 6 CUDA fixture).
//
// Goal: let clang's `-x cuda -nocudainc -nocudalib --cuda-host-only` mode
// (kg's "nowrapper" CUDA rung -- see kg::index::cuda::synthesize()) parse a
// `.cu` file containing `__global__`/`__device__`/`__shared__` declarations
// and a `kernel<<<grid,block>>>(args)` launch, WITHOUT any real CUDA
// toolkit installed. Force-included via `-include` (see
// compile_commands.json.in / test/CMakeLists.txt's cuda_fixture_index test).
//
// This header's exact contents were arrived at empirically, by iterating
// directly against clang (not by trusting a recipe blindly) -- see
// docs/KNOWN_LIMITATIONS.md's Phase 6 section for the full iteration log.
// Three non-obvious findings from that process, preserved here as comments
// since they're exactly the kind of thing a future maintainer would
// otherwise have to rediscover the hard way:
//
//   1. `-nocudainc` disables clang's own bundled CUDA wrapper headers (the
//      ones that normally declare `threadIdx`/`blockIdx`/`blockDim`/
//      `gridDim` as compiler-magic builtins) -- clang does NOT provide
//      these automatically in this mode. They must be declared here.
//   2. clang's Sema lowers a `kernel<<<g,b>>>(args)` launch into a call to
//      `cudaConfigureCall(dim3 gridDim, dim3 blockDim, size_t sharedMem = 0,
//      cudaStream_t stream = 0)` (and, if that returns nonzero, a direct
//      call to `kernel(args)`). This EXACT signature -- `size_t`, not
//      `unsigned long`, and both trailing parameters defaulted -- must be
//      declared, or you get "use of undeclared identifier
//      'cudaConfigureCall'" (with no defaults: "too few execution
//      configuration arguments to kernel function call, expected 4, have
//      2"). `__cudaPushCallConfiguration` is NOT used by this lowering
//      path (empirically confirmed by deleting cudaConfigureCall's
//      declaration and observing the compile error name it directly, not
//      __cudaPushCallConfiguration).
//   3. `size_t` itself is not a builtin type name in this mode -- plain
//      `-nocudainc -nocudalib` does not stop `<cstddef>` (or any other
//      ordinary C++ standard header) from being found via the normal
//      `-resource-dir`/system include search, so `#include <cstddef>` is
//      enough (no need to hand-roll a `size_t` typedef).
#pragma once
#include <cstddef>

#define __global__ __attribute__((global))
#define __device__ __attribute__((device))
#define __host__ __attribute__((host))
#define __shared__ __attribute__((shared))
#define __constant__ __attribute__((constant))

struct dim3
{
    unsigned int x, y, z;
    dim3(unsigned int vx = 1, unsigned int vy = 1, unsigned int vz = 1) : x(vx), y(vy), z(vz) {}
};

struct uint3
{
    unsigned int x, y, z;
};

// threadIdx/blockIdx/blockDim/gridDim: NOT provided automatically by
// `-nocudainc` mode (see finding #1 above) -- declared as ordinary
// __device__-space externs, sufficient for Sema to type-check a kernel body
// that reads `.x`/`.y`/`.z` off them (no real device codegen happens under
// --cuda-host-only, so no defined storage/intrinsic backing is needed).
extern "C" __device__ uint3 threadIdx;
extern "C" __device__ uint3 blockIdx;
extern "C" __device__ dim3 blockDim;
extern "C" __device__ dim3 gridDim;

typedef int cudaError_t;
struct CUstream_st;
typedef CUstream_st* cudaStream_t;

// See finding #2 above: this exact signature (size_t, both trailing
// parameters defaulted) is what clang's <<<>>> lowering requires.
extern "C" cudaError_t cudaConfigureCall(dim3 gridDim, dim3 blockDim, size_t sharedMem = 0, cudaStream_t stream = 0);
extern "C" cudaError_t cudaGetLastError(void);
