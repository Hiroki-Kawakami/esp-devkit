/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <cstddef>
#include <cstdint>

#include "esp_err.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "usb/usb_host.h"

namespace usb_host::detail {

class TransferContext {
public:
    TransferContext() = default;
    ~TransferContext();
    TransferContext(const TransferContext&) = delete;
    TransferContext& operator=(const TransferContext&) = delete;

    esp_err_t init(usb_host_client_handle_t client);
    usb_host_client_handle_t client() const { return client_; }
    usb_device_handle_t device() const { return device_; }
    void set_device(usb_device_handle_t device) { device_ = device; }

    // Blocks until the transfer completes; one transfer in flight per context.
    esp_err_t submit(usb_transfer_t* transfer, uint8_t endpoint, size_t bytes,
                     uint32_t timeout_ms);
    esp_err_t control(usb_transfer_t* transfer, uint8_t request_type, uint8_t request,
                      uint16_t value, uint16_t index, uint16_t length, uint32_t timeout_ms);
    esp_err_t clear_halt(usb_transfer_t* control_transfer, uint8_t endpoint,
                         uint32_t timeout_ms);

    bool borrowable(const void* buffer, size_t bytes, uint16_t mps) const;
    static void set_buffer(usb_transfer_t* transfer, void* buffer, size_t bytes);
    static void sync_for_device(void* buffer, size_t bytes);
    static void sync_for_cpu(void* buffer, size_t bytes);

private:
    static void done(usb_transfer_t* transfer);
    esp_err_t await(usb_transfer_t* transfer, uint32_t timeout_ms);

    usb_host_client_handle_t client_ = nullptr;
    usb_device_handle_t device_ = nullptr;
    SemaphoreHandle_t done_ = nullptr;
    size_t dma_alignment_ = 4;
    size_t psram_alignment_ = 4;
};

}  // namespace usb_host::detail
