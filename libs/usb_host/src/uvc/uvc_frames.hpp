/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <mutex>
#include <vector>

#include "esp_err.h"
#include "usb_host_uvc.hpp"

namespace usb_host::detail {

// The caller's slots, filled by one producer and handed out by receive().
class UvcFrameQueue {
public:
    void reset(uint8_t* const* slots, size_t count, size_t bytes);
    void clear();
    void abort();

    // Null when every slot is filling or held.
    uint8_t* begin(size_t* capacity);
    void commit(size_t size);
    void cancel();

    esp_err_t receive(UvcFrame* frame, uint32_t timeout_ms);
    void release(const UvcFrame& frame);

    struct Counts {
        int free;
        int filling;
        int ready;
        int held;
    };
    Counts counts();

private:
    enum class State : uint8_t { Free, Filling, Ready, Held };

    struct Slot {
        uint8_t* data = nullptr;
        size_t size = 0;
        State state = State::Free;
    };

    std::mutex lock_;
    std::condition_variable ready_;
    std::vector<Slot> slots_;
    size_t bytes_ = 0;
    int filling_ = -1;
    int ready_slot_ = -1;
    uint32_t session_ = 0;
    bool running_ = false;
    bool aborted_ = false;
};

}  // namespace usb_host::detail
