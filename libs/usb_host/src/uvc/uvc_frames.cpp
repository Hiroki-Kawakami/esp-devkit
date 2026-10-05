/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "uvc_frames.hpp"

#include <algorithm>
#include <chrono>

namespace usb_host {

bool UvcFrameSize::supports(uint32_t interval) const {
    if (!intervals.empty()) {
        return std::find(intervals.begin(), intervals.end(), interval) != intervals.end();
    }
    if (interval < min_interval || interval > max_interval) return false;
    return step_interval == 0 || (interval - min_interval) % step_interval == 0;
}

namespace detail {

void UvcFrameQueue::reset(uint8_t* const* slots, size_t count, size_t bytes) {
    std::lock_guard<std::mutex> guard(lock_);
    slots_.assign(count, Slot{});
    for (size_t i = 0; i < count; i++) slots_[i].data = slots[i];
    bytes_ = bytes;
    filling_ = -1;
    ready_slot_ = -1;
    session_++;
    running_ = true;
}

void UvcFrameQueue::clear() {
    {
        std::lock_guard<std::mutex> guard(lock_);
        slots_.clear();
        filling_ = -1;
        ready_slot_ = -1;
        session_++;
        running_ = false;
    }
    ready_.notify_all();
}

void UvcFrameQueue::abort() {
    {
        std::lock_guard<std::mutex> guard(lock_);
        aborted_ = true;
    }
    ready_.notify_all();
}

uint8_t* UvcFrameQueue::begin(size_t* capacity) {
    std::lock_guard<std::mutex> guard(lock_);
    if (!running_) return nullptr;
    if (filling_ >= 0) slots_[filling_].state = State::Free;
    filling_ = -1;
    for (size_t i = 0; i < slots_.size(); i++) {
        if (slots_[i].state == State::Free) {
            filling_ = static_cast<int>(i);
            break;
        }
    }
    if (filling_ < 0 && ready_slot_ >= 0) {
        filling_ = ready_slot_;
        ready_slot_ = -1;
    }
    if (filling_ < 0) return nullptr;
    slots_[filling_].state = State::Filling;
    *capacity = bytes_;
    return slots_[filling_].data;
}

void UvcFrameQueue::commit(size_t size) {
    {
        std::lock_guard<std::mutex> guard(lock_);
        if (filling_ < 0) return;
        if (ready_slot_ >= 0) slots_[ready_slot_].state = State::Free;
        slots_[filling_].state = State::Ready;
        slots_[filling_].size = size;
        ready_slot_ = filling_;
        filling_ = -1;
    }
    ready_.notify_one();
}

void UvcFrameQueue::cancel() {
    std::lock_guard<std::mutex> guard(lock_);
    if (filling_ < 0) return;
    slots_[filling_].state = State::Free;
    filling_ = -1;
}

esp_err_t UvcFrameQueue::receive(UvcFrame* frame, uint32_t timeout_ms) {
    std::unique_lock<std::mutex> guard(lock_);
    const uint32_t session = session_;
    const bool woken = ready_.wait_for(guard, std::chrono::milliseconds(timeout_ms), [&] {
        return aborted_ || session_ != session || ready_slot_ >= 0;
    });
    if (aborted_) return ESP_ERR_NOT_FOUND;
    if (!running_ || session_ != session) return ESP_ERR_INVALID_STATE;
    if (!woken) return ESP_ERR_TIMEOUT;
    Slot& slot = slots_[ready_slot_];
    slot.state = State::Held;
    frame->data = slot.data;
    frame->size = slot.size;
    frame->slot = ready_slot_;
    frame->session = session_;
    ready_slot_ = -1;
    return ESP_OK;
}

void UvcFrameQueue::release(const UvcFrame& frame) {
    std::lock_guard<std::mutex> guard(lock_);
    if (frame.session != session_ || frame.slot < 0 ||
        frame.slot >= static_cast<int>(slots_.size())) {
        return;
    }
    Slot& slot = slots_[frame.slot];
    if (slot.state == State::Held) slot.state = State::Free;
}

UvcFrameQueue::Counts UvcFrameQueue::counts() {
    std::lock_guard<std::mutex> guard(lock_);
    Counts counts = {};
    for (const Slot& slot : slots_) {
        switch (slot.state) {
            case State::Free: counts.free++; break;
            case State::Filling: counts.filling++; break;
            case State::Ready: counts.ready++; break;
            case State::Held: counts.held++; break;
        }
    }
    return counts;
}

}  // namespace detail

}  // namespace usb_host
