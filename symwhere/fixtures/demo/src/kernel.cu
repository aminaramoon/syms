#include "ring_buffer.hpp"

__global__ void accumulate(const float* in, float* out, int n) {
    int idx = blockIdx.x * blockDim.x + threadIdx.x;
    if (idx < n) out[idx] += in[idx];
}

void launch_accumulate(const float* in, float* out, int n) {
    accumulate<<<256, 256>>>(in, out, n);
}
