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

struct InStreamConfig {
    uint8_t endpoint = 0;
    bool isochronous = false;
    uint16_t max_packet_bytes = 0;
    // Isochronous: (micro)frames between packets and packets per transfer.
    uint32_t interval = 1;
    int packets = 0;
    // Bulk: bytes per transfer, a multiple of max_packet_bytes.
    size_t transfer_bytes = 0;
    int transfers = 0;
};

// Transfers stay queued and are resubmitted from their completion on the
// client task, where the sink runs too.
class InStream {
public:
    // One isochronous packet or one bulk transfer. `ok` false is a packet lost
    // on the bus; `requested` tells a short bulk transfer from a full one.
    using Sink = void (*)(void* context, const uint8_t* data, size_t len, size_t requested,
                          bool ok);

    InStream() = default;
    ~InStream();
    InStream(const InStream&) = delete;
    InStream& operator=(const InStream&) = delete;

    esp_err_t init();
    esp_err_t start(usb_device_handle_t device, const InStreamConfig& config, Sink sink,
                    void* context);
    // Returns once no transfer is in flight.
    void stop();

    struct Stats {
        uint32_t transfers;
        uint32_t packets_ok;
        uint32_t packets_skipped;
        uint32_t packets_failed;
        uint32_t packets_error;
        uint32_t packets_overflow;
        uint32_t packets_stall;
        int failed_bytes_min;
        int failed_bytes_max;
        uint32_t not_resubmitted;
        int inflight;
        int64_t last_done_us;
        uint8_t last_status;
    };
    // Counters since the previous call.
    Stats take_stats();

private:
    static constexpr int kMaxTransfers = 4;

    static void done(usb_transfer_t* transfer);
    void deliver(usb_transfer_t* transfer);
    void release_one();
    void free_transfers();

    usb_device_handle_t device_ = nullptr;
    InStreamConfig config_;
    Sink sink_ = nullptr;
    void* context_ = nullptr;
    usb_transfer_t* transfers_[kMaxTransfers] = {};
    // The host stack's own buffers, put back before the transfers are freed.
    uint8_t* own_buffers_[kMaxTransfers] = {};
    size_t own_bytes_[kMaxTransfers] = {};
    std::atomic<int> inflight_{0};
    std::atomic<uint32_t> transfers_done_{0};
    std::atomic<uint32_t> packets_ok_{0};
    std::atomic<uint32_t> packets_skipped_{0};
    std::atomic<uint32_t> packets_failed_{0};
    std::atomic<uint32_t> packets_error_{0};
    std::atomic<uint32_t> packets_overflow_{0};
    std::atomic<uint32_t> packets_stall_{0};
    std::atomic<int> failed_bytes_min_{-1};
    std::atomic<int> failed_bytes_max_{-1};
    std::atomic<uint32_t> not_resubmitted_{0};
    std::atomic<int64_t> last_done_us_{0};
    std::atomic<uint8_t> last_status_{0};
    std::atomic<bool> running_{false};
    SemaphoreHandle_t idle_ = nullptr;
};

}  // namespace usb_host::detail
