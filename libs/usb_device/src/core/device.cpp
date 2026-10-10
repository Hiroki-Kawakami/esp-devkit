/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <algorithm>
#include <cstring>
#include <utility>
#include <vector>

#include "dcd.hpp"
#include "descriptors.hpp"
#include "esp_log.h"
#include "freertos/semphr.h"
#include "freertos/task.h"
#include "usb_device.hpp"

namespace usb_device {

using detail::Event;
using detail::EventType;
using detail::SetupPacket;

namespace {

const char* TAG = "usb_device";

constexpr int kEventQueueLength = 16;
constexpr uint32_t kTaskStackBytes = 4096;
constexpr size_t kMinControlBytes = 1024;
constexpr uint32_t kDisconnectHoldMs = 100;
constexpr uint8_t kConfigurationValue = 1;
constexpr uint8_t kStatusSelfPowered = 0x01;
constexpr uint8_t kStatusHalted = 0x01;
constexpr uint16_t kFeatureEndpointHalt = 0x00;
constexpr uint16_t kMsOs20DescriptorIndex = 7;

bool has_alternate(const std::vector<uint8_t>& config, uint8_t interface, uint8_t alternate) {
    for (size_t pos = 0; pos + 3 < config.size() && config[pos] > 0; pos += config[pos]) {
        if (config[pos + 1] == detail::kDescInterface && config[pos + 2] == interface &&
            config[pos + 3] == alternate) {
            return true;
        }
    }
    return false;
}

ControlRequest to_request(const SetupPacket& setup) {
    return {setup.bmRequestType, setup.bRequest, setup.wValue, setup.wIndex, setup.wLength};
}

}  // namespace

struct Device::State {
    DeviceConfig config;
    std::vector<std::shared_ptr<Function>> functions;
    detail::Descriptors descriptors;
    detail::Dcd dcd;
    QueueHandle_t events = nullptr;
    TaskHandle_t task = nullptr;
    SemaphoreHandle_t stopped = nullptr;
    std::vector<uint8_t> alternates;
    size_t control_bytes = 0;
    SetupPacket pending_out = {};
    volatile Speed speed = Speed::Full;
    volatile uint8_t configuration = 0;
    bool started = false;

    static void task_main(void* arg);
    void complete_transfers();
    void handle_setup(const SetupPacket& setup);
    bool handle_standard(const SetupPacket& setup);
    void handle_function_request(const SetupPacket& setup);
    void handle_control_out(uint16_t length);
    bool call_functions(const SetupPacket& setup, uint8_t* data, size_t* bytes);
    bool get_descriptor(const SetupPacket& setup);
    bool set_configuration(uint8_t value);
    bool set_interface(uint8_t interface, uint8_t alternate);
    void unconfigure();
    void open_alternate(uint8_t interface, uint8_t alternate);
    void close_alternate(uint8_t interface);
    const detail::EndpointInfo* active_endpoint(uint8_t address) const;
    void send(const void* data, size_t bytes, uint16_t requested);
};

void Device::State::task_main(void* arg) {
    auto* state = static_cast<State*>(arg);
    Event event;
    while (true) {
        xQueueReceive(state->events, &event, portMAX_DELAY);
        switch (event.type) {
            case EventType::Reset:
                state->unconfigure();
                break;
            case EventType::Enumerated:
                state->speed = event.speed;
                ESP_LOGI(TAG, "%s speed", event.speed == Speed::High ? "high" : "full");
                break;
            case EventType::Suspend:
            case EventType::Resume:
                break;
            case EventType::Setup:
                state->handle_setup(event.setup);
                break;
            case EventType::Ep0Received:
                state->handle_control_out(event.length);
                break;
            case EventType::TransfersDone:
                break;
            case EventType::Stop:
                state->unconfigure();
                state->complete_transfers();
                xSemaphoreGive(state->stopped);
                vTaskDelete(nullptr);
                return;
        }
        state->complete_transfers();
    }
}

void Device::State::complete_transfers() {
    while (Transfer* transfer = dcd.take_done()) {
        if (transfer->callback) transfer->callback(transfer);
    }
}

void Device::State::send(const void* data, size_t bytes, uint16_t requested) {
    const size_t length = std::min<size_t>(bytes, requested);
    if (data) memcpy(dcd.ep0_buffer(), data, length);
    const bool zlp = length < requested && length % detail::kEp0MaxPacket == 0;
    dcd.ep0_send(length, zlp);
}

bool Device::State::get_descriptor(const SetupPacket& setup) {
    const uint8_t type = setup.wValue >> 8;
    const uint8_t index = setup.wValue & 0xff;
    const bool high_speed_port = config.port == Port::HighSpeed;
    const Speed other = speed == Speed::High ? Speed::Full : Speed::High;

    switch (type) {
        case detail::kDescDevice:
            send(descriptors.device.data(), descriptors.device.size(), setup.wLength);
            return true;
        case detail::kDescConfig:
            if (index != 0) return false;
            send(descriptors.config_for(speed).data(), descriptors.config_for(speed).size(),
                 setup.wLength);
            return true;
        case detail::kDescString: {
            const size_t bytes = detail::string_descriptor(descriptors, index, dcd.ep0_buffer(),
                                                           control_bytes);
            if (bytes == 0) return false;
            send(nullptr, bytes, setup.wLength);
            return true;
        }
        case detail::kDescBos:
            if (descriptors.bos.empty()) return false;
            send(descriptors.bos.data(), descriptors.bos.size(), setup.wLength);
            return true;
        case detail::kDescDeviceQualifier:
            if (!high_speed_port) return false;
            send(descriptors.qualifier.data(), descriptors.qualifier.size(), setup.wLength);
            return true;
        case detail::kDescOtherSpeedConfig: {
            if (!high_speed_port || index != 0) return false;
            const std::vector<uint8_t>& config_bytes = descriptors.config_for(other);
            const size_t length = std::min<size_t>(config_bytes.size(), setup.wLength);
            uint8_t* buffer = dcd.ep0_buffer();
            memcpy(buffer, config_bytes.data(), length);
            if (length > 1) buffer[1] = detail::kDescOtherSpeedConfig;
            send(nullptr, config_bytes.size(), setup.wLength);
            return true;
        }
        default:
            return false;
    }
}

const detail::EndpointInfo* Device::State::active_endpoint(uint8_t address) const {
    if (configuration == 0) return nullptr;
    for (const auto& endpoint : descriptors.endpoints_for(speed)) {
        if (endpoint.address == address &&
            endpoint.alternate == alternates[endpoint.interface]) {
            return &endpoint;
        }
    }
    return nullptr;
}

void Device::State::open_alternate(uint8_t interface, uint8_t alternate) {
    for (const auto& endpoint : descriptors.endpoints_for(speed)) {
        if (endpoint.interface != interface || endpoint.alternate != alternate) continue;
        const esp_err_t err =
            dcd.open_endpoint(endpoint.address, endpoint.type, endpoint.max_packet_bytes);
        if (err != ESP_OK) {
            ESP_LOGE(TAG, "open endpoint 0x%02x: %s", endpoint.address, esp_err_to_name(err));
        }
    }
}

void Device::State::close_alternate(uint8_t interface) {
    for (const auto& endpoint : descriptors.endpoints_for(speed)) {
        if (endpoint.interface == interface && endpoint.alternate == alternates[interface]) {
            dcd.close_endpoint(endpoint.address);
        }
    }
}

void Device::State::unconfigure() {
    if (configuration == 0) return;
    for (const auto& function : functions) function->unconfigured();
    for (size_t i = 0; i < alternates.size(); i++) close_alternate(static_cast<uint8_t>(i));
    configuration = 0;
}

bool Device::State::set_configuration(uint8_t value) {
    if (value != 0 && value != kConfigurationValue) return false;
    unconfigure();
    if (value == 0) return true;
    std::fill(alternates.begin(), alternates.end(), 0);
    for (size_t i = 0; i < alternates.size(); i++) open_alternate(static_cast<uint8_t>(i), 0);
    configuration = value;
    for (const auto& function : functions) function->configured();
    ESP_LOGI(TAG, "configured");
    return true;
}

bool Device::State::set_interface(uint8_t interface, uint8_t alternate) {
    if (!has_alternate(descriptors.config_for(speed), interface, alternate)) return false;
    const uint8_t previous = alternates[interface];
    close_alternate(interface);
    alternates[interface] = alternate;
    open_alternate(interface, alternate);
    if (descriptors.interface_owner[interface]->set_alternate(interface, alternate)) return true;
    close_alternate(interface);
    alternates[interface] = previous;
    open_alternate(interface, previous);
    return false;
}

bool Device::State::handle_standard(const SetupPacket& setup) {
    const uint8_t recipient = setup.bmRequestType & detail::kReqRecipMask;
    const auto endpoint = static_cast<uint8_t>(setup.wIndex);
    const bool ep0 = (endpoint & 0x7f) == 0;

    switch (setup.bRequest) {
        case detail::kReqGetDescriptor:
            return recipient == detail::kReqRecipDevice && get_descriptor(setup);
        case detail::kReqSetAddress:
            if (recipient != detail::kReqRecipDevice || setup.wValue > 127) return false;
            dcd.set_address(static_cast<uint8_t>(setup.wValue));
            dcd.ep0_ack();
            return true;
        case detail::kReqGetConfiguration: {
            const uint8_t value = configuration;
            send(&value, 1, setup.wLength);
            return true;
        }
        case detail::kReqSetConfiguration:
            if (!set_configuration(static_cast<uint8_t>(setup.wValue))) return false;
            dcd.ep0_ack();
            return true;
        case detail::kReqGetStatus: {
            uint8_t status[2] = {};
            if (recipient == detail::kReqRecipDevice) {
                if (config.info.self_powered) status[0] = kStatusSelfPowered;
            } else if (recipient == detail::kReqRecipInterface) {
                if (configuration == 0 || setup.wIndex >= alternates.size()) return false;
            } else if (recipient == detail::kReqRecipEndpoint) {
                if (!ep0 && !active_endpoint(endpoint)) return false;
                if (!ep0 && dcd.stalled(endpoint)) status[0] = kStatusHalted;
            } else {
                return false;
            }
            send(status, sizeof(status), setup.wLength);
            return true;
        }
        case detail::kReqClearFeature:
        case detail::kReqSetFeature:
            if (recipient != detail::kReqRecipEndpoint || setup.wValue != kFeatureEndpointHalt) {
                return false;
            }
            if (!ep0) {
                if (!active_endpoint(endpoint)) return false;
                dcd.set_stall(endpoint, setup.bRequest == detail::kReqSetFeature);
            }
            dcd.ep0_ack();
            return true;
        case detail::kReqGetInterface:
            if (recipient != detail::kReqRecipInterface || configuration == 0 ||
                setup.wIndex >= alternates.size()) {
                return false;
            }
            send(&alternates[setup.wIndex], 1, setup.wLength);
            return true;
        case detail::kReqSetInterface:
            if (recipient != detail::kReqRecipInterface || configuration == 0 ||
                setup.wIndex >= alternates.size()) {
                return false;
            }
            if (!set_interface(static_cast<uint8_t>(setup.wIndex),
                               static_cast<uint8_t>(setup.wValue))) {
                return false;
            }
            dcd.ep0_ack();
            return true;
        default:
            return false;
    }
}

bool Device::State::call_functions(const SetupPacket& setup, uint8_t* data, size_t* bytes) {
    const ControlRequest request = to_request(setup);
    const size_t room = *bytes;
    switch (setup.bmRequestType & detail::kReqRecipMask) {
        case detail::kReqRecipInterface: {
            const uint8_t interface = setup.wIndex & 0xff;
            if (configuration == 0 || interface >= alternates.size()) return false;
            return descriptors.interface_owner[interface]->control(request, data, bytes);
        }
        case detail::kReqRecipEndpoint: {
            const detail::EndpointInfo* endpoint = active_endpoint(setup.wIndex & 0xff);
            if (!endpoint) return false;
            return descriptors.interface_owner[endpoint->interface]->control(request, data,
                                                                             bytes);
        }
        case detail::kReqRecipDevice:
            for (const auto& function : functions) {
                *bytes = room;
                if (function->control(request, data, bytes)) return true;
            }
            return false;
        default:
            return false;
    }
}

void Device::State::handle_function_request(const SetupPacket& setup) {
    if (!descriptors.ms_os_20.empty() &&
        setup.bmRequestType == (detail::kReqDirIn | detail::kReqTypeVendor) &&
        setup.bRequest == config.info.ms_os_20_vendor_code &&
        setup.wIndex == kMsOs20DescriptorIndex) {
        send(descriptors.ms_os_20.data(), descriptors.ms_os_20.size(), setup.wLength);
        return;
    }
    if (setup.bmRequestType & detail::kReqDirIn) {
        size_t bytes = control_bytes;
        if (!call_functions(setup, dcd.ep0_buffer(), &bytes)) {
            dcd.ep0_stall();
            return;
        }
        send(nullptr, std::min(bytes, control_bytes), setup.wLength);
    } else if (setup.wLength == 0) {
        size_t bytes = 0;
        if (call_functions(setup, dcd.ep0_buffer(), &bytes)) {
            dcd.ep0_ack();
        } else {
            dcd.ep0_stall();
        }
    } else if (setup.wLength > control_bytes) {
        dcd.ep0_stall();
    } else {
        pending_out = setup;
        dcd.ep0_receive(setup.wLength);
    }
}

void Device::State::handle_control_out(uint16_t length) {
    size_t bytes = length;
    if (call_functions(pending_out, dcd.ep0_buffer(), &bytes)) {
        dcd.ep0_ack();
    } else {
        dcd.ep0_stall();
    }
}

void Device::State::handle_setup(const SetupPacket& setup) {
    if ((setup.bmRequestType & detail::kReqTypeMask) != detail::kReqTypeStandard) {
        handle_function_request(setup);
    } else if (!handle_standard(setup)) {
        dcd.ep0_stall();
    }
}

esp_err_t Endpoint::submit(Transfer* transfer) {
    if (!device_ || !transfer) return ESP_ERR_INVALID_ARG;
    transfer->endpoint = address_;
    return device_->state_->dcd.submit(transfer);
}

void Endpoint::flush() {
    if (device_) device_->state_->dcd.flush(address_);
}

esp_err_t Endpoint::set_stall(bool stall) {
    if (!device_) return ESP_ERR_INVALID_ARG;
    return device_->state_->dcd.set_stall(address_, stall);
}

Device::Device(DeviceConfig config) : state_(std::make_unique<State>()) {
    state_->config = std::move(config);
}

Device::~Device() {
    stop();
}

esp_err_t Device::add(std::shared_ptr<Function> function) {
    if (!function) return ESP_ERR_INVALID_ARG;
    if (state_->started) return ESP_ERR_INVALID_STATE;
    function->device_ = this;
    state_->functions.push_back(std::move(function));
    return ESP_OK;
}

esp_err_t Device::start() {
    State& s = *state_;
    if (s.started) return ESP_ERR_INVALID_STATE;

    esp_err_t err = detail::build_descriptors(s.config.info, s.config.port, s.functions,
                                              &s.descriptors);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "descriptors: %s", esp_err_to_name(err));
        return err;
    }
    s.alternates.assign(s.descriptors.interface_owner.size(), 0);
    s.control_bytes = std::max(s.descriptors.largest(), kMinControlBytes);

    s.events = xQueueCreate(kEventQueueLength, sizeof(Event));
    s.stopped = xSemaphoreCreateBinary();
    if (!s.events || !s.stopped) return ESP_ERR_NO_MEM;

    err = s.dcd.start(s.config.port, s.control_bytes, s.events);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "controller: %s", esp_err_to_name(err));
        return err;
    }

    const BaseType_t core = s.config.task_core < 0 ? tskNO_AFFINITY : s.config.task_core;
    if (xTaskCreatePinnedToCore(State::task_main, "usb_device", kTaskStackBytes, &s,
                                s.config.task_priority, &s.task, core) != pdPASS) {
        s.dcd.stop();
        return ESP_ERR_NO_MEM;
    }
    s.started = true;
    return ESP_OK;
}

void Device::stop() {
    State& s = *state_;
    if (s.started) {
        const Event event{EventType::Stop, Speed::Full, 0, {}};
        xQueueSend(s.events, &event, portMAX_DELAY);
        xSemaphoreTake(s.stopped, portMAX_DELAY);
        s.dcd.stop();
        s.started = false;
        // Reconnecting at once looks like a bus reset to the host, which then
        // keeps the descriptors it read before.
        vTaskDelay(pdMS_TO_TICKS(kDisconnectHoldMs));
    }
    if (s.events) vQueueDelete(s.events);
    if (s.stopped) vSemaphoreDelete(s.stopped);
    s.events = nullptr;
    s.stopped = nullptr;
}

bool Device::configured() const {
    return state_->configuration != 0;
}

Speed Device::speed() const {
    return state_->speed;
}

}  // namespace usb_device
