/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <algorithm>
#include <mutex>
#include <string>
#include <utility>
#include <vector>

#include "esp_log.h"
#include "esp_vfs_fat.h"
#include "host_internal.hpp"
#include "msc_bot_device.hpp"
#include "usb_host_msc.hpp"

namespace usb_host {

namespace {

const char* TAG = "usb_host_msc";

struct Mount {
    std::string path;
    std::shared_ptr<detail::MscBotDevice> device;
};

std::mutex s_lock;
std::vector<std::shared_ptr<detail::MscBotDevice>> s_devices;
std::vector<Mount> s_mounts;

std::vector<Mount>::iterator find_mount(const char* mount_point) {
    return std::find_if(s_mounts.begin(), s_mounts.end(),
                        [&](const Mount& mount) { return mount.path == mount_point; });
}

}  // namespace

esp_err_t detail::msc_install() {
    return ESP_OK;
}

void detail::msc_connected(uint8_t address) {
    std::shared_ptr<MscBotDevice> device;
    const esp_err_t err = MscBotDevice::open(client(), address, &device);
    if (err != ESP_OK) {
        if (err != ESP_ERR_NOT_SUPPORTED) ESP_LOGE(TAG, "open: %s", esp_err_to_name(err));
        return;
    }
    {
        std::lock_guard<std::mutex> guard(s_lock);
        s_devices.push_back(device);
    }
    if (callbacks().msc_connected) callbacks().msc_connected(device);
}

void detail::msc_gone(usb_device_handle_t handle) {
    std::shared_ptr<MscBotDevice> device;
    {
        std::lock_guard<std::mutex> guard(s_lock);
        auto it = std::find_if(s_devices.begin(), s_devices.end(),
                               [&](const auto& entry) { return entry->usb_device() == handle; });
        if (it == s_devices.end()) return;
        device = std::move(*it);
        s_devices.erase(it);
    }
    device->mark_gone();
    if (callbacks().msc_disconnected) callbacks().msc_disconnected(device);
}

esp_err_t mount(std::shared_ptr<MscDevice> device, const char* mount_point, uint8_t max_files) {
    if (!device || !mount_point) return ESP_ERR_INVALID_ARG;
    if (!device->connected()) return ESP_ERR_NOT_FOUND;
    auto bot = std::static_pointer_cast<detail::MscBotDevice>(std::move(device));

    std::lock_guard<std::mutex> guard(s_lock);
    if (find_mount(mount_point) != s_mounts.end()) return ESP_ERR_INVALID_STATE;
    for (const Mount& mount : s_mounts) {
        if (mount.device == bot) return ESP_ERR_INVALID_STATE;
    }
    esp_vfs_fat_mount_config_t config = {};
    config.format_if_mount_failed = false;
    config.max_files = max_files > 0 ? max_files : 5;
    const esp_err_t err = esp_vfs_fat_bdl_mount(mount_point, bot->blockdev(), &config);
    if (err != ESP_OK) return err;
    s_mounts.push_back({mount_point, std::move(bot)});
    return ESP_OK;
}

esp_err_t unmount(const char* mount_point) {
    if (!mount_point) return ESP_ERR_INVALID_ARG;
    Mount released;  // dropped after the lock: the last reference closes the USB device
    esp_err_t err = ESP_OK;
    {
        std::lock_guard<std::mutex> guard(s_lock);
        auto it = find_mount(mount_point);
        if (it == s_mounts.end()) return ESP_ERR_INVALID_STATE;
        err = esp_vfs_fat_bdl_unmount(mount_point, it->device->blockdev());
        released = std::move(*it);
        s_mounts.erase(it);
    }
    return err;
}

bool mounted(const char* mount_point) {
    if (!mount_point) return false;
    std::lock_guard<std::mutex> guard(s_lock);
    auto it = find_mount(mount_point);
    return it != s_mounts.end() && it->device->connected();
}

}  // namespace usb_host
