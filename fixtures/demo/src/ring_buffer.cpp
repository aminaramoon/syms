#include "ring_buffer.hpp"

namespace dsp {

RingBuffer<T>::RingBuffer(std::size_t capacity)
    : data_(new T[capacity]), capacity_(capacity), size_(0), head_(0) {}

void RingBuffer<T>::push(const T& value) {
    if (size_ == capacity_) throw Status::Overflow;
    data_[head_] = value;
    head_ = (head_ + 1) % capacity_;
    ++size_;
}

T RingBuffer<T>::pop() {
    if (size_ == 0) throw Status::Empty;
    --size_;
    return data_[(head_ - size_ + capacity_) % capacity_];
}

namespace detail {
int compute_latency(int frames) { return frames * kMaxChannels; }
}  // namespace detail

}  // namespace dsp
