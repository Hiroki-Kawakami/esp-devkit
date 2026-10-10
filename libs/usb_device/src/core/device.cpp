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
constexpr uint8_t kConfigurationValue = 1;
constexpr uint8_t kStatusSelfPowered = 0x01;

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
    volatile Speed speed = Speed::Full;
    volatile uint8_t configuration = 0;
    bool started = false;

    static void task_main(void* arg);
    void handle_setup(const SetupPacket& setup);
    bool get_descriptor(const SetupPacket& setup);
    bool set_configuration(uint8_t value);
    void unconfigure();
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
            case EventType::Stop:
                state->unconfigure();
                xSemaphoreGive(state->stopped);
                vTaskDelete(nullptr);
                return;
        }
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
                                                           descriptors.largest());
            if (bytes == 0) return false;
            send(nullptr, bytes, setup.wLength);
            return true;
        }
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

void Device::State::unconfigure() {
    if (configuration == 0) return;
    configuration = 0;
    for (const auto& function : functions) function->unconfigured();
}

bool Device::State::set_configuration(uint8_t value) {
    if (value != 0 && value != kConfigurationValue) return false;
    unconfigure();
    if (value == 0) return true;
    std::fill(alternates.begin(), alternates.end(), 0);
    configuration = value;
    for (const auto& function : functions) function->configured();
    ESP_LOGI(TAG, "configured");
    return true;
}

void Device::State::handle_setup(const SetupPacket& setup) {
    const uint8_t recipient = setup.bmRequestType & detail::kReqRecipMask;
    if ((setup.bmRequestType & detail::kReqTypeMask) != detail::kReqTypeStandard) {
        dcd.ep0_stall();
        return;
    }

    const uint8_t zero[2] = {};
    bool handled = false;
    switch (setup.bRequest) {
        case detail::kReqGetDescriptor:
            handled = recipient == detail::kReqRecipDevice && get_descriptor(setup);
            break;
        case detail::kReqSetAddress:
            if (recipient != detail::kReqRecipDevice || setup.wValue > 127) break;
            dcd.set_address(static_cast<uint8_t>(setup.wValue));
            dcd.ep0_ack();
            handled = true;
            break;
        case detail::kReqGetConfiguration: {
            const uint8_t value = configuration;
            send(&value, 1, setup.wLength);
            handled = true;
            break;
        }
        case detail::kReqSetConfiguration:
            handled = set_configuration(static_cast<uint8_t>(setup.wValue));
            if (handled) dcd.ep0_ack();
            break;
        case detail::kReqGetStatus:
            if (recipient == detail::kReqRecipDevice) {
                const uint8_t status[2] = {
                    static_cast<uint8_t>(config.info.self_powered ? kStatusSelfPowered : 0), 0};
                send(status, sizeof(status), setup.wLength);
                handled = true;
            } else if (recipient == detail::kReqRecipInterface) {
                if (configuration == 0 || setup.wIndex >= alternates.size()) break;
                send(zero, sizeof(zero), setup.wLength);
                handled = true;
            } else if (recipient == detail::kReqRecipEndpoint) {
                if ((setup.wIndex & 0x7f) != 0) break;
                send(zero, sizeof(zero), setup.wLength);
                handled = true;
            }
            break;
        case detail::kReqGetInterface:
            if (recipient != detail::kReqRecipInterface || configuration == 0 ||
                setup.wIndex >= alternates.size()) {
                break;
            }
            send(&alternates[setup.wIndex], 1, setup.wLength);
            handled = true;
            break;
        case detail::kReqSetInterface: {
            if (recipient != detail::kReqRecipInterface || configuration == 0 ||
                setup.wIndex >= alternates.size()) {
                break;
            }
            const auto interface = static_cast<uint8_t>(setup.wIndex);
            const auto alternate = static_cast<uint8_t>(setup.wValue);
            if (!descriptors.interface_owner[interface]->set_alternate(interface, alternate)) {
                break;
            }
            alternates[interface] = alternate;
            dcd.ep0_ack();
            handled = true;
            break;
        }
        default:
            break;
    }
    if (!handled) dcd.ep0_stall();
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

    s.events = xQueueCreate(kEventQueueLength, sizeof(Event));
    s.stopped = xSemaphoreCreateBinary();
    if (!s.events || !s.stopped) return ESP_ERR_NO_MEM;

    err = s.dcd.start(s.config.port, s.descriptors.largest(), s.events);
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
        s.dcd.stop();
        const Event event{EventType::Stop, Speed::Full, {}};
        xQueueSend(s.events, &event, portMAX_DELAY);
        xSemaphoreTake(s.stopped, portMAX_DELAY);
        s.started = false;
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
