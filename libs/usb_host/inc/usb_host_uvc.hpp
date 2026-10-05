/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <cstddef>
#include <cstdint>
#include <string>
#include <vector>

#include "esp_err.h"
#include "usb_host.hpp"

namespace usb_host {

// Intervals are in 100 ns units. Empty intervals means the continuous range
// min_interval..max_interval in steps of step_interval.
struct UvcFrameSize {
    uint16_t width = 0;
    uint16_t height = 0;
    std::vector<uint32_t> intervals;
    uint32_t min_interval = 0;
    uint32_t max_interval = 0;
    uint32_t step_interval = 0;

    bool supports(uint32_t interval) const;
};

struct UvcFrame {
    const uint8_t* data = nullptr;
    size_t size = 0;
    int slot = -1;
    uint32_t session = 0;
};

// An MJPEG camera. Outlives its connection while anyone holds it; every call
// fails once unplugged.
class UvcDevice {
public:
    virtual ~UvcDevice() = default;
    virtual bool connected() const = 0;
    // The product string, empty when the device has none.
    virtual const std::string& name() const = 0;
    virtual const std::vector<UvcFrameSize>& frame_sizes() const = 0;

    // Each of the caller's slots takes one JPEG; a frame larger than
    // slot_bytes is dropped. Starting a running stream restarts it.
    virtual esp_err_t start(uint16_t width, uint16_t height, uint32_t interval,
                            uint8_t* const* slots, size_t count, size_t slot_bytes) = 0;
    // Frames still held stay readable; releasing them afterwards is a no-op.
    virtual void stop() = 0;
    // The newest complete frame; one that was never received is dropped for
    // a newer one. Hold it until release(), which gives the slot back.
    virtual esp_err_t receive(UvcFrame* frame, uint32_t timeout_ms) = 0;
    virtual void release(const UvcFrame& frame) = 0;
};

}  // namespace usb_host
