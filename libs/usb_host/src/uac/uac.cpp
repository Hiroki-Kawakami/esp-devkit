/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <algorithm>
#include <mutex>
#include <vector>

#include "esp_log.h"
#include "host_internal.hpp"
#include "uac_capture.hpp"
#include "uac_device.hpp"

namespace usb_host {

namespace {

const char* TAG = "usb_host_uac";

std::mutex s_lock;
std::vector<std::shared_ptr<detail::UacOutputDevice>> s_devices;
std::vector<std::shared_ptr<detail::UacInputDevice>> s_captures;

template <typename Device>
std::shared_ptr<Device> take(std::vector<std::shared_ptr<Device>>* devices,
                             usb_device_handle_t handle) {
    std::lock_guard<std::mutex> guard(s_lock);
    auto it = std::find_if(devices->begin(), devices->end(),
                           [&](const auto& entry) { return entry->usb_device() == handle; });
    if (it == devices->end()) return nullptr;
    std::shared_ptr<Device> device = std::move(*it);
    devices->erase(it);
    return device;
}

}  // namespace

esp_err_t detail::uac_install() {
    return ESP_OK;
}

void detail::uac_connected(uint8_t address) {
    std::shared_ptr<UacOutputDevice> device;
    esp_err_t err = UacOutputDevice::open(client(), address, &device);
    if (err == ESP_OK) {
        {
            std::lock_guard<std::mutex> guard(s_lock);
            s_devices.push_back(device);
        }
        if (callbacks().uac_connected) callbacks().uac_connected(device);
    } else if (err != ESP_ERR_NOT_SUPPORTED) {
        ESP_LOGE(TAG, "open: %s", esp_err_to_name(err));
    }

    std::shared_ptr<UacInputDevice> capture;
    err = UacInputDevice::open(client(), address, &capture);
    if (err == ESP_OK) {
        {
            std::lock_guard<std::mutex> guard(s_lock);
            s_captures.push_back(capture);
        }
        if (callbacks().uac_capture_connected) callbacks().uac_capture_connected(capture);
    } else if (err != ESP_ERR_NOT_SUPPORTED) {
        ESP_LOGE(TAG, "open capture: %s", esp_err_to_name(err));
    }
}

void detail::uac_gone(usb_device_handle_t handle) {
    if (auto device = take(&s_devices, handle)) {
        device->mark_gone();
        if (callbacks().uac_disconnected) callbacks().uac_disconnected(device);
    }
    if (auto capture = take(&s_captures, handle)) {
        capture->mark_gone();
        if (callbacks().uac_capture_disconnected) callbacks().uac_capture_disconnected(capture);
    }
}

}  // namespace usb_host
