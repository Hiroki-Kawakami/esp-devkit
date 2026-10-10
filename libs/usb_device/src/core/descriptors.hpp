/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <cstddef>
#include <cstdint>
#include <memory>
#include <string>
#include <vector>

#include "esp_err.h"
#include "usb_device.hpp"

namespace usb_device::detail {

struct Descriptors {
    std::vector<uint8_t> device;
    std::vector<uint8_t> qualifier;
    std::vector<uint8_t> config[2];  // by Speed; empty when the port cannot run at it
    std::vector<std::string> strings;  // index 1 onwards
    std::vector<Function*> interface_owner;

    const std::vector<uint8_t>& config_for(Speed speed) const {
        return config[static_cast<int>(speed)];
    }
    size_t largest() const;
};

struct ConfigState {
    Speed speed;
    std::vector<uint8_t>& bytes;
    std::vector<std::string>& strings;
    size_t interface_offset = 0;
    uint8_t interfaces = 0;
    uint8_t next_out = 1;
    uint8_t next_in = 1;
    bool error = false;
};

esp_err_t build_descriptors(const DeviceInfo& info, Port port,
                            const std::vector<std::shared_ptr<Function>>& functions,
                            Descriptors* out);

// Writes string descriptor `index` (0 is the language list) into `out`.
size_t string_descriptor(const Descriptors& descriptors, uint8_t index, uint8_t* out,
                         size_t capacity);

}  // namespace usb_device::detail
