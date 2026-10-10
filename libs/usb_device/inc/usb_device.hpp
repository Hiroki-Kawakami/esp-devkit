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
enum class TransferStatus : uint8_t { Completed, Canceled, Stalled, Error };

struct DeviceInfo {
    uint16_t vendor_id = 0;
    uint16_t product_id = 0;
    uint16_t bcd_device = 0;
    std::string manufacturer;
    std::string product;
    std::string serial;
    uint16_t max_power_ma = 100;
    bool self_powered = false;
    // Raises bcdUSB to 2.1 and answers BOS and the MS OS 2.0 descriptor set,
    // which carries what functions ask for through ConfigBuilder::winusb().
    bool ms_os_20 = false;
    uint8_t ms_os_20_vendor_code = 0x01;
};

struct DeviceConfig {
    Port port = Port::HighSpeed;
    DeviceInfo info;
    int task_priority = 5;
    int task_core = -1;
};

struct ControlRequest {
    uint8_t request_type;
    uint8_t request;
    uint16_t value;
    uint16_t index;
    uint16_t length;
};

// The controller reads and writes `buffer` directly. An OUT buffer must start
// and end on a cache line and be a multiple of the endpoint's max packet
// size; an IN buffer needs 4-byte alignment. Either may be in PSRAM.
// Completion runs `callback` on the device task.
struct Transfer {
    uint8_t* buffer = nullptr;
    uint32_t length = 0;
    uint32_t actual = 0;
    TransferStatus status = TransferStatus::Completed;
    // IN: end with a zero-length packet when `length` is a multiple of the
    // max packet size.
    bool zlp = false;
    void (*callback)(Transfer* transfer) = nullptr;
    void* context = nullptr;

    // Owned by the stack while queued.
    Transfer* next = nullptr;
    uint8_t endpoint = 0;
};

class Device;

class Endpoint {
public:
    Endpoint() = default;

    uint8_t address() const { return address_; }
    // Transfers queue up and run in order. Fails while the endpoint is not
    // part of the active configuration.
    esp_err_t submit(Transfer* transfer);
    // Queued transfers come back canceled, with what they moved so far.
    void flush();
    esp_err_t set_stall(bool stall);

private:
    friend class Function;
    Endpoint(Device* device, uint8_t address) : device_(device), address_(address) {}
    Device* device_ = nullptr;
    uint8_t address_ = 0;
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
    // Binds WinUSB to the function being described, when DeviceInfo::ms_os_20
    // is set. `device_interface_guid` is "{...}", or empty for none.
    void winusb(std::string_view device_interface_guid = {});

private:
    detail::ConfigState& state_;
};

class Function {
public:
    virtual ~Function() = default;

    // Called once per speed the port supports, so it must declare the same
    // interfaces and endpoints in the same order every time.
    virtual void describe(ConfigBuilder& config) = 0;
    // These run on the device task. Endpoints of the active alternates are
    // open by the time they are called.
    virtual void configured() {}
    virtual void unconfigured() {}
    // false STALLs the SET_INTERFACE.
    virtual bool set_alternate(uint8_t interface, uint8_t alternate) { return alternate == 0; }
    // Class and vendor requests to the function's interfaces or endpoints,
    // and those to the device that no earlier function took. For OUT, `data`
    // holds what the host sent; for IN, `*bytes` is the room in `data` and is
    // set to the reply length. false STALLs.
    virtual bool control(const ControlRequest& request, uint8_t* data, size_t* bytes) {
        return false;
    }

protected:
    Endpoint endpoint(uint8_t address) const { return Endpoint(device_, address); }
    Device* device() const { return device_; }

private:
    friend class Device;
    Device* device_ = nullptr;
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
    friend class Endpoint;
    struct State;
    std::unique_ptr<State> state_;
};

}  // namespace usb_device
