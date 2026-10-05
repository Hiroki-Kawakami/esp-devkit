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
#include "usb_core.hpp"

namespace usb_host::detail {

class TransferContext {
public:
    TransferContext() = default;
    ~TransferContext();
    TransferContext(const TransferContext&) = delete;
    TransferContext& operator=(const TransferContext&) = delete;

    esp_err_t init();
    Device* device() const { return device_; }
    void set_device(Device* device) { device_ = device; }

    // Blocks until the transfer completes; one transfer in flight per context.
    esp_err_t submit(Transfer* transfer, uint8_t endpoint, size_t bytes, uint32_t timeout_ms);
    esp_err_t control(Transfer* transfer, uint8_t request_type, uint8_t request, uint16_t value,
                      uint16_t index, uint16_t length, uint32_t timeout_ms);
    esp_err_t clear_halt(Transfer* control_transfer, uint8_t endpoint, uint32_t timeout_ms);

    bool borrowable(const void* buffer, size_t bytes, uint16_t mps) const;

private:
    static void done(Transfer* transfer);
    esp_err_t await(Transfer* transfer, uint32_t timeout_ms);

    Device* device_ = nullptr;
    SemaphoreHandle_t done_ = nullptr;
    size_t dma_alignment_ = 4;
    size_t psram_alignment_ = 4;
};

}  // namespace usb_host::detail
