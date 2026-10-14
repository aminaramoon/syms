// Synthetic CUDA fixture TU for the hermetic "nowrapper" rung CTest
// (test_cuda_fixture.cpp / cuda_fixture_index -- see test/CMakeLists.txt).
// Parsed with `-x cuda -nocudainc -nocudalib --cuda-host-only -include
// fake_cuda.h`, so it must not depend on any real CUDA toolkit header.

// __device__ helper called BY the kernel below (exercises a CALL edge
// entirely within device code).
__device__ int symhow_cuda_fixture_double_value(int v)
{
    return v * 2;
}

// __global__ kernel: uses __shared__ memory and threadIdx/blockIdx/blockDim,
// and calls the __device__ helper above.
__global__ void symhow_cuda_fixture_kernel(int* data, int n)
{
    __shared__ int cache[32];
    const int idx = threadIdx.x + blockIdx.x * blockDim.x;
    if (idx < n)
    {
        cache[threadIdx.x % 32] = data[idx];
        data[idx] = symhow_cuda_fixture_double_value(cache[threadIdx.x % 32]);
    }
}

// Host-side launcher: calls the kernel via <<<grid, block>>>(...) syntax
// (exercises the CALL edge the test asserts on).
void symhow_cuda_fixture_launch(int* data, int n)
{
    dim3 block(32, 1, 1);
    dim3 grid(static_cast<unsigned int>((n + 31) / 32), 1, 1);
    symhow_cuda_fixture_kernel<<<grid, block>>>(data, n);
}
