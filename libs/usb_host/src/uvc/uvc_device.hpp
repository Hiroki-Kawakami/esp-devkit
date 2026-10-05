/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <atomic>
#include <cstdint>
#include <memory>
#include <mutex>
#include <vector>

#include "esp_timer.h"
#include "in_stream.hpp"
#include "transfer.hpp"
#include "usb_host_uvc.hpp"
#include "uvc_descriptors.hpp"
#include "uvc_frames.hpp"

namespace usb_host::detail {

class UvcCameraDevice final : public UvcDevice {
public:
    static esp_err_t open(usb_host_client_handle_t client, uint8_t address,
                          std::shared_ptr<UvcCameraDevice>* out);
    ~UvcCameraDevice() override;

    bool connected() const override { return !gone_; }
    const std::string& name() const override { return name_; }
    const std::vector<UvcFrameSize>& frame_sizes() const override { return sizes_; }
    esp_err_t start(uint16_t width, uint16_t height, uint32_t interval, uint8_t* const* slots,
                    size_t count, size_t slot_bytes) override;
    void stop() override;
    esp_err_t receive(UvcFrame* frame, uint32_t timeout_ms) override;
    void release(const UvcFrame& frame) override;

    usb_device_handle_t usb_device() const { return xfer_.device(); }
    void mark_gone();

private:
    UvcCameraDevice() = default;
    esp_err_t setup(usb_host_client_handle_t client, uint8_t address);
    esp_err_t negotiate(const UvcFrameDesc& frame, uint32_t interval, uint32_t* max_payload);
    esp_err_t start_isochronous(uint32_t max_payload);
    esp_err_t start_bulk(uint32_t max_payload);
    esp_err_t control(uint8_t request_type, uint8_t request, uint16_t value, uint16_t index,
                      void* data, uint16_t length);
    void stop_locked();

    static void on_data(void* context, const uint8_t* data, size_t len, size_t requested,
                        bool ok);
    void on_isochronous(const uint8_t* data, size_t len, bool ok);
    void on_bulk(const uint8_t* data, size_t len, size_t requested, bool ok);
    bool payload_start(const uint8_t* data, size_t len, size_t* header);
    void append(const uint8_t* data, size_t len);
    void payload_end();
    void finish_frame();
    static void log_stats(void* arg);

    TransferContext xfer_;
    usb_transfer_t* ctrl_ = nullptr;
    UvcTopology topology_;
    std::string name_;
    std::vector<UvcFrameSize> sizes_;
    InStream stream_;
    UvcFrameQueue frames_;
    std::mutex lock_;
    std::atomic<bool> gone_{false};
    usb_speed_t speed_ = USB_SPEED_FULL;
    bool streaming_ = false;
    bool claimed_ = false;
    bool isochronous_ = false;

    // Client task only.
    uint8_t header_info_ = 0;
    int fid_ = -1;
    bool assembling_ = false;
    bool skip_ = false;
    bool overflowed_ = false;
    uint8_t* frame_ = nullptr;
    size_t frame_bytes_ = 0;
    size_t frame_capacity_ = 0;
    bool in_payload_ = false;
    size_t payload_bytes_ = 0;
    size_t max_payload_ = 0;

    esp_timer_handle_t stats_timer_ = nullptr;
    std::atomic<uint32_t> committed_{0};
    std::atomic<uint32_t> dropped_error_{0};
    std::atomic<uint32_t> dropped_overflow_{0};
    std::atomic<uint32_t> dropped_not_jpeg_{0};
    std::atomic<uint32_t> dropped_no_slot_{0};
    std::atomic<uint32_t> bad_headers_{0};
    std::atomic<uint32_t> fid_toggles_{0};
    std::atomic<int64_t> last_commit_us_{0};
};

}  // namespace usb_host::detail
