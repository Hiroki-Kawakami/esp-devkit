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

#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "in_stream.hpp"
#include "transfer.hpp"
#include "uac_descriptors.hpp"
#include "usb_host_uac.hpp"

namespace usb_host::detail {

class UacInputDevice final : public UacCaptureDevice {
public:
    static esp_err_t open(usb_host_client_handle_t client, uint8_t address,
                          std::shared_ptr<UacInputDevice>* out);
    ~UacInputDevice() override;

    bool connected() const override { return !gone_; }
    const std::vector<UacFormat>& formats() const override { return formats_; }
    esp_err_t open(size_t format, uint32_t rate) override;
    void close() override;
    esp_err_t read(void* data, size_t len, size_t* read, uint32_t timeout_ms) override;
    size_t available() const override;

    usb_device_handle_t usb_device() const { return xfer_.device(); }
    void mark_gone();

private:
    UacInputDevice() = default;
    esp_err_t setup(usb_host_client_handle_t client, uint8_t address);
    void apply_quirks(uint16_t vendor, uint16_t product);
    esp_err_t control_out(uint8_t request_type, uint8_t request, uint16_t value, uint16_t index,
                          const void* data, uint16_t length);
    void close_locked();

    static void on_data(void* context, const uint8_t* data, size_t len, size_t requested,
                        bool ok);

    TransferContext xfer_;
    usb_transfer_t* ctrl_ = nullptr;
    UacTopology topology_;
    std::vector<UacFormat> formats_;
    // Per format, the rate to ask the device for when it declares one and
    // sends another; 0 where they agree.
    std::vector<uint32_t> declared_rates_;
    InStream stream_;
    std::mutex lock_;
    std::atomic<bool> gone_{false};
    usb_speed_t speed_ = USB_SPEED_FULL;
    int active_ = -1;

    uint8_t* ring_ = nullptr;
    size_t ring_bytes_ = 0;
    size_t frame_bytes_ = 0;
    std::atomic<size_t> read_{0};
    std::atomic<size_t> written_{0};
    SemaphoreHandle_t data_ = nullptr;
};

}  // namespace usb_host::detail
