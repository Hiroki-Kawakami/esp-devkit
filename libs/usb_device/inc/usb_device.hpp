/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <cstddef>
#include <cstdint>
#include <memory>
#include <string>
#include <string_view>

#include "esp_err.h"

namespace usb_device {

namespace detail {
struct ConfigState;
}

enum class Port : uint8_t { HighSpeed, FullSpeed };
enum class Speed : uint8_t { Full, High };
enum class EndpointType : uint8_t { Control = 0, Isochronous = 1, Bulk = 2, Interrupt = 3 };

struct DeviceInfo {
    uint16_t vendor_id = 0;
    uint16_t product_id = 0;
    uint16_t bcd_device = 0;
    std::string manufacturer;
    std::string product;
    std::string serial;
    uint16_t max_power_ma = 100;
    bool self_powered = false;
};

struct DeviceConfig {
    Port port = Port::HighSpeed;
    DeviceInfo info;
    int task_priority = 5;
    int task_core = -1;
};

class ConfigBuilder {
public:
    explicit ConfigBuilder(detail::ConfigState& state) : state_(state) {}

    Speed speed() const;
    // Opens alternate 0 of a new interface and returns its number.
    uint8_t interface(uint8_t cls, uint8_t subclass, uint8_t protocol,
                      std::string_view name = {});
    // Endpoints that follow belong to this alternate.
    void alternate(uint8_t interface, uint8_t alternate, uint8_t cls, uint8_t subclass,
                   uint8_t protocol, std::string_view name = {});
    // Returns the endpoint address, direction bit included.
    uint8_t endpoint(EndpointType type, bool in, uint16_t max_packet_bytes, uint8_t interval = 0);
    void append(const void* data, size_t bytes);
    uint8_t string(std::string_view text);

private:
    detail::ConfigState& state_;
};

class Function {
public:
    virtual ~Function() = default;

    // Called once per speed the port supports, so it must declare the same
    // interfaces and endpoints in the same order every time.
    virtual void describe(ConfigBuilder& config) = 0;
    // Run on the device task.
    virtual void configured() {}
    virtual void unconfigured() {}
    // false STALLs the SET_INTERFACE.
    virtual bool set_alternate(uint8_t interface, uint8_t alternate) { return alternate == 0; }
};

class Device {
public:
    explicit Device(DeviceConfig config);
    ~Device();
    Device(const Device&) = delete;
    Device& operator=(const Device&) = delete;

    // Functions are added before start(), and described in the order added.
    esp_err_t add(std::shared_ptr<Function> function);
    esp_err_t start();
    void stop();

    bool configured() const;
    Speed speed() const;

private:
    struct State;
    std::unique_ptr<State> state_;
};

}  // namespace usb_device
