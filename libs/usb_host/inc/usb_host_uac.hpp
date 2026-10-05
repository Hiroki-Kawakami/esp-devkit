/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <cstddef>
#include <cstdint>
#include <vector>

#include "esp_err.h"
#include "usb_host.hpp"

namespace usb_host {

// Samples are little-endian and MSB-justified in subframe_bytes.
struct UacFormat {
    uint8_t channels = 0;
    uint8_t subframe_bytes = 0;
    uint8_t bit_resolution = 0;
    // Empty rates means the continuous range min_rate..max_rate.
    std::vector<uint32_t> rates;
    uint32_t min_rate = 0;
    uint32_t max_rate = 0;

    bool supports(uint32_t rate) const;
};

// Outlives its connection while anyone holds it; every call fails once unplugged.
class UacDevice {
public:
    virtual ~UacDevice() = default;
    virtual bool connected() const = 0;
    virtual const std::vector<UacFormat>& formats() const = 0;

    // `format` indexes formats(); opening a running stream reopens it.
    virtual esp_err_t open(size_t format, uint32_t rate) = 0;
    virtual void close() = 0;
    // Blocks while the stream buffer is full.
    virtual esp_err_t write(const void* data, size_t len) = 0;
    virtual void drain() = 0;

    virtual bool has_volume() const = 0;
    virtual bool has_mute() const = 0;
    virtual float volume_min_db() const = 0;
    virtual float volume_max_db() const = 0;
    // Never block: the latest value goes out once the control pipe is free.
    // The volume is clamped to what the device accepts.
    virtual void set_volume_db(float db) = 0;
    virtual void set_mute(bool mute) = 0;
};

// Records PCM from a device's capture interface. Same lifetime rules as
// UacDevice.
class UacCaptureDevice {
public:
    virtual ~UacCaptureDevice() = default;
    virtual bool connected() const = 0;
    // What the device sends, which for a few devices is not what it declares.
    virtual const std::vector<UacFormat>& formats() const = 0;

    virtual esp_err_t open(size_t format, uint32_t rate) = 0;
    virtual void close() = 0;
    // Waits up to timeout_ms for at least one frame and copies whole frames
    // only; must not overlap close(). Data that arrives while the buffer is
    // full is dropped.
    virtual esp_err_t read(void* data, size_t len, size_t* read, uint32_t timeout_ms) = 0;
    // Bytes recorded and not read yet.
    virtual size_t available() const = 0;
};

}  // namespace usb_host
