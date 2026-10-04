/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <cstdint>
#include <memory>

#include "esp_err.h"
#include "usb_host.hpp"

namespace usb_host {

// Outlives its connection while anyone holds it; I/O fails once unplugged.
class MscDevice {
public:
    virtual ~MscDevice() = default;
    virtual bool connected() const = 0;
    virtual uint32_t block_size() const = 0;
    virtual uint32_t block_count() const = 0;
    virtual esp_err_t read(void* dst, uint32_t block, uint32_t count) = 0;
    virtual esp_err_t write(const void* src, uint32_t block, uint32_t count) = 0;
};

// FAT on the whole device. A mount keeps its device alive after unplugging and
// stays registered until unmount(); mounted() is false for such a mount.
esp_err_t mount(std::shared_ptr<MscDevice> device, const char* mount_point,
                uint8_t max_files = 0);
esp_err_t unmount(const char* mount_point);
bool mounted(const char* mount_point);

}  // namespace usb_host
