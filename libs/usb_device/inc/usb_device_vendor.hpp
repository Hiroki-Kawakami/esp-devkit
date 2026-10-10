/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <functional>
#include <string>

#include "usb_device.hpp"

namespace usb_device {

// One vendor-specific interface with a bulk OUT and/or a bulk IN endpoint.
class Vendor : public Function {
public:
    struct Config {
        std::string name;
        bool out = true;
        bool in = true;
        // See ConfigBuilder::winusb().
        bool winusb = false;
        std::string device_interface_guid;
        // Run on the device task.
        std::function<void()> configured;
        std::function<void()> unconfigured;
        std::function<bool(const ControlRequest&, uint8_t* data, size_t* bytes)> control;
    };

    explicit Vendor(Config config) : config_(std::move(config)) {}

    Endpoint out() const { return endpoint(out_address_); }
    Endpoint in() const { return endpoint(in_address_); }

    void describe(ConfigBuilder& config) override;
    void configured() override;
    void unconfigured() override;
    bool control(const ControlRequest& request, uint8_t* data, size_t* bytes) override;

private:
    Config config_;
    uint8_t interface_ = 0;
    uint8_t out_address_ = 0;
    uint8_t in_address_ = 0;
};

}  // namespace usb_device
