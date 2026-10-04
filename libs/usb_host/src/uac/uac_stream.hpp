/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <atomic>
#include <cstddef>
#include <cstdint>

#include "esp_err.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "usb/usb_host.h"

namespace usb_host::detail {

class PacketClock {
public:
    void start(uint32_t rate, uint32_t period_us);
    uint32_t next();
    uint32_t max_frames() const;

private:
    uint32_t rate_ = 0;
    uint32_t period_us_ = 0;
    uint64_t carry_ = 0;
};

struct IsocOutConfig {
    uint8_t endpoint = 0;
    uint16_t max_packet_bytes = 0;
    uint32_t period_us = 0;
    uint32_t interval = 1;
    uint32_t rate = 0;
    size_t frame_bytes = 0;
};

class IsocOutStream {
public:
    IsocOutStream() = default;
    ~IsocOutStream();
    IsocOutStream(const IsocOutStream&) = delete;
    IsocOutStream& operator=(const IsocOutStream&) = delete;

    esp_err_t init();
    esp_err_t start(usb_device_handle_t device, const IsocOutConfig& config);
    // Returns once no transfer is in flight.
    void stop();
    // The device is gone: a blocked write() returns at once.
    void abort();
    esp_err_t write(const void* data, size_t len);
    void drain();

private:
    static constexpr int kTransfers = 3;

    static void done(usb_transfer_t* transfer);
    void fill(usb_transfer_t* transfer);
    void release_one();
    void free_buffers();

    PacketClock clock_;
    usb_device_handle_t device_ = nullptr;
    usb_transfer_t* transfers_[kTransfers] = {};
    int packets_ = 0;
    size_t frame_bytes_ = 0;
    uint32_t transfer_ms_ = 0;
    uint8_t* ring_ = nullptr;
    size_t ring_bytes_ = 0;
    std::atomic<size_t> read_{0};
    std::atomic<size_t> written_{0};
    std::atomic<int> inflight_{0};
    std::atomic<bool> running_{false};
    std::atomic<bool> stopping_{false};
    std::atomic<bool> aborted_{false};
    SemaphoreHandle_t space_ = nullptr;
    SemaphoreHandle_t idle_ = nullptr;
};

}  // namespace usb_host::detail
