/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <sys/stat.h>

#include <algorithm>
#include <atomic>
#include <cstdlib>
#include <mutex>
#include <string>
#include <utility>
#include <vector>

#include "harness.h"
#include "host_internal.hpp"
#include "simulator/path_redirect.h"
#include "usb_host_msc.hpp"

namespace usb_host {

namespace {

class SimMscDevice final : public MscDevice {
public:
    explicit SimMscDevice(std::string host_dir) : host_dir_(std::move(host_dir)) {}

    bool connected() const override { return !gone_; }
    uint32_t block_size() const override { return 0; }
    uint32_t block_count() const override { return 0; }
    esp_err_t read(void*, uint32_t, uint32_t) override { return ESP_ERR_NOT_SUPPORTED; }
    esp_err_t write(const void*, uint32_t, uint32_t) override { return ESP_ERR_NOT_SUPPORTED; }

    const std::string& host_dir() const { return host_dir_; }
    void mark_gone() { gone_ = true; }

private:
    std::string host_dir_;
    std::atomic<bool> gone_{false};
};

struct Mount {
    std::string path;
    std::shared_ptr<SimMscDevice> device;
    bool redirected;
};

std::mutex s_lock;
std::shared_ptr<SimMscDevice> s_device;
std::vector<Mount> s_mounts;

std::vector<Mount>::iterator find_mount(const char* mount_point) {
    return std::find_if(s_mounts.begin(), s_mounts.end(),
                        [&](const Mount& mount) { return mount.path == mount_point; });
}

bool is_directory(const char* path) {
    struct stat st;
    return stat(path, &st) == 0 && S_ISDIR(st.st_mode);
}

esp_err_t attach(const char* host_dir) {
    if (!is_directory(host_dir)) return ESP_ERR_INVALID_ARG;
    std::shared_ptr<SimMscDevice> device;
    {
        std::lock_guard<std::mutex> guard(s_lock);
        if (s_device) return s_device->host_dir() == host_dir ? ESP_OK : ESP_ERR_INVALID_STATE;
        s_device = std::make_shared<SimMscDevice>(host_dir);
        device = s_device;
    }
    if (detail::callbacks().msc_connected) detail::callbacks().msc_connected(device);
    return ESP_OK;
}

void detach() {
    std::shared_ptr<SimMscDevice> device;
    {
        std::lock_guard<std::mutex> guard(s_lock);
        if (!s_device) return;
        device = std::move(s_device);
        device->mark_gone();
        for (Mount& mount : s_mounts) {
            if (mount.device != device || !mount.redirected) continue;
            sim_path_redirect_remove(mount.path.c_str());
            mount.redirected = false;
        }
    }
    if (detail::callbacks().msc_disconnected) detail::callbacks().msc_disconnected(device);
}

const char* default_host_dir() {
    const char* dir = getenv("SIMULATOR_USBH_MSC_PATH");
    return dir ? dir : "simulator/usb";
}

bool cmd_attach(int argc, const char* const* argv, void*) {
    const char* dir = argc > 1 ? argv[1] : default_host_dir();
    const esp_err_t err = attach(dir);
    if (err == ESP_ERR_INVALID_ARG) {
        harness_reply("ERR %s: not a directory: %s", argv[0], dir);
    } else if (err != ESP_OK) {
        harness_reply("ERR %s: another directory is attached", argv[0]);
    }
    return true;
}

bool cmd_detach(int, const char* const*, void*) {
    detach();
    return true;
}

}  // namespace

esp_err_t detail::msc_install() {
    harness_register("usbh-msc-attach", cmd_attach, nullptr);
    harness_register("usbh-msc-detach", cmd_detach, nullptr);
    if (getenv("SIMULATOR_USBH_MSC_PATH")) attach(default_host_dir());
    return ESP_OK;
}

esp_err_t mount(std::shared_ptr<MscDevice> device, const char* mount_point, uint8_t) {
    if (!device || !mount_point) return ESP_ERR_INVALID_ARG;
    if (!device->connected()) return ESP_ERR_NOT_FOUND;
    auto sim = std::static_pointer_cast<SimMscDevice>(std::move(device));

    std::lock_guard<std::mutex> guard(s_lock);
    if (find_mount(mount_point) != s_mounts.end()) return ESP_ERR_INVALID_STATE;
    for (const Mount& mount : s_mounts) {
        if (mount.device == sim) return ESP_ERR_INVALID_STATE;
    }
    const esp_err_t err = sim_path_redirect_add(mount_point, sim->host_dir().c_str());
    if (err != ESP_OK) return err;
    s_mounts.push_back({mount_point, std::move(sim), true});
    return ESP_OK;
}

esp_err_t unmount(const char* mount_point) {
    if (!mount_point) return ESP_ERR_INVALID_ARG;
    std::lock_guard<std::mutex> guard(s_lock);
    auto it = find_mount(mount_point);
    if (it == s_mounts.end()) return ESP_ERR_INVALID_STATE;
    if (it->redirected) sim_path_redirect_remove(mount_point);
    s_mounts.erase(it);
    return ESP_OK;
}

bool mounted(const char* mount_point) {
    if (!mount_point) return false;
    std::lock_guard<std::mutex> guard(s_lock);
    auto it = find_mount(mount_point);
    return it != s_mounts.end() && it->device->connected();
}

}  // namespace usb_host
