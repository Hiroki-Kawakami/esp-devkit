/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <algorithm>
#include <mutex>
#include <vector>

#include "esp_log.h"
#include "host_internal.hpp"
#include "uac_device.hpp"

namespace usb_host {

namespace {

const char* TAG = "usb_host_uac";

std::mutex s_lock;
std::vector<std::shared_ptr<detail::UacOutputDevice>> s_devices;

}  // namespace

esp_err_t detail::uac_install() {
    return ESP_OK;
}

void detail::uac_connected(uint8_t address) {
    std::shared_ptr<UacOutputDevice> device;
    const esp_err_t err = UacOutputDevice::open(client(), address, &device);
    if (err != ESP_OK) {
        if (err != ESP_ERR_NOT_SUPPORTED) ESP_LOGE(TAG, "open: %s", esp_err_to_name(err));
        return;
    }
    {
        std::lock_guard<std::mutex> guard(s_lock);
        s_devices.push_back(device);
    }
    if (callbacks().uac_connected) callbacks().uac_connected(device);
}

void detail::uac_gone(usb_device_handle_t handle) {
    std::shared_ptr<UacOutputDevice> device;
    {
        std::lock_guard<std::mutex> guard(s_lock);
        auto it = std::find_if(s_devices.begin(), s_devices.end(),
                               [&](const auto& entry) { return entry->usb_device() == handle; });
        if (it == s_devices.end()) return;
        device = std::move(*it);
        s_devices.erase(it);
    }
    device->mark_gone();
    if (callbacks().uac_disconnected) callbacks().uac_disconnected(device);
}

}  // namespace usb_host
