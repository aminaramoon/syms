#pragma once
#include <cstddef>

namespace dsp {

/// Fixed-capacity ring buffer.
template <typename T>
class RingBuffer {
public:
    explicit RingBuffer(std::size_t capacity);
    void push(const T& value);
    T pop();
    std::size_t size() const { return size_; }

private:
    T* data_;
    std::size_t capacity_;
    std::size_t size_;
    std::size_t head_;
};

enum class Status { Ok, Overflow, Empty };

struct Sample {
    float left;
    float right;
};

using SampleBuffer = RingBuffer<Sample>;

constexpr int kMaxChannels = 8;

}  // namespace dsp
