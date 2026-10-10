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

struct EndpointInfo {
    uint8_t address;
    EndpointType type;
    uint16_t max_packet_bytes;
    uint8_t interface;
    uint8_t alternate;
};

struct WinUsbFunction {
    uint8_t first_interface;
    std::string guid;
};

struct Descriptors {
    std::vector<uint8_t> device;
    std::vector<uint8_t> qualifier;
    // By Speed; empty when the port cannot run at it.
    std::vector<uint8_t> config[2];
    std::vector<EndpointInfo> endpoints[2];
    std::vector<std::string> strings;  // index 1 onwards
    std::vector<Function*> interface_owner;
    std::vector<WinUsbFunction> winusb;
    std::vector<uint8_t> bos;
    std::vector<uint8_t> ms_os_20;

    const std::vector<uint8_t>& config_for(Speed speed) const {
        return config[static_cast<int>(speed)];
    }
    const std::vector<EndpointInfo>& endpoints_for(Speed speed) const {
        return endpoints[static_cast<int>(speed)];
    }
    size_t largest() const;
};

struct ConfigState {
    Speed speed;
    std::vector<uint8_t>& bytes;
    std::vector<EndpointInfo>& endpoints;
    std::vector<std::string>& strings;
    std::vector<WinUsbFunction>& winusb;
    size_t interface_offset = 0;
    uint8_t function_first = 0;
    uint8_t interfaces = 0;
    uint8_t interface = 0;
    uint8_t alternate = 0;
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
