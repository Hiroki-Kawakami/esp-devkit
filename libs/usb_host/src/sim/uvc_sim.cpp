/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <mutex>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#include "esp_log.h"
#include "harness.h"
#include "host_internal.hpp"
#include "usb_host_uvc.hpp"
#include "uvc_frames.hpp"

namespace usb_host {

namespace {

const char* TAG = "usb_host_uvc";

using Clock = std::chrono::steady_clock;

constexpr uint32_t kIntervals[] = {333333, 666666};

class SimUvcDevice final : public UvcDevice {
public:
    SimUvcDevice(std::vector<std::vector<uint8_t>> pictures, std::vector<UvcFrameSize> sizes)
        : pictures_(std::move(pictures)), sizes_(std::move(sizes)) {}
    ~SimUvcDevice() override { stop(); }

    bool connected() const override { return !gone_; }
    const std::string& name() const override { return name_; }
    const std::vector<UvcFrameSize>& frame_sizes() const override { return sizes_; }

    esp_err_t start(uint16_t width, uint16_t height, uint32_t interval, uint8_t* const* slots,
                    size_t count, size_t slot_bytes) override {
        std::lock_guard<std::mutex> guard(lock_);
        if (gone_) return ESP_ERR_NOT_FOUND;
        if (!slots || !count || !slot_bytes) return ESP_ERR_INVALID_ARG;
        auto size = std::find_if(sizes_.begin(), sizes_.end(), [&](const UvcFrameSize& entry) {
            return entry.width == width && entry.height == height;
        });
        if (size == sizes_.end() || !size->supports(interval)) return ESP_ERR_NOT_SUPPORTED;
        stop_locked();
        frames_.reset(slots, count, slot_bytes);
        running_ = true;
        thread_ = std::thread([this, interval] { run(interval); });
        ESP_LOGI(TAG, "sim start %ux%u, interval %u", width, height, static_cast<unsigned>(interval));
        return ESP_OK;
    }

    void stop() override {
        std::lock_guard<std::mutex> guard(lock_);
        stop_locked();
    }

    esp_err_t receive(UvcFrame* frame, uint32_t timeout_ms) override {
        if (gone_) return ESP_ERR_NOT_FOUND;
        return frames_.receive(frame, timeout_ms);
    }

    void release(const UvcFrame& frame) override { frames_.release(frame); }

    void mark_gone() {
        gone_ = true;
        frames_.abort();
    }

private:
    void stop_locked() {
        running_ = false;
        if (thread_.joinable()) thread_.join();
        frames_.clear();
    }

    void run(uint32_t interval) {
        const auto period = std::chrono::nanoseconds(static_cast<int64_t>(interval) * 100);
        auto due = Clock::now();
        size_t next = 0;
        while (running_ && !gone_) {
            const std::vector<uint8_t>& picture = pictures_[next];
            next = (next + 1) % pictures_.size();
            size_t capacity = 0;
            if (uint8_t* slot = frames_.begin(&capacity)) {
                if (picture.size() <= capacity) {
                    memcpy(slot, picture.data(), picture.size());
                    frames_.commit(picture.size());
                } else {
                    frames_.cancel();
                }
            }
            due += period;
            std::this_thread::sleep_until(due);
        }
    }

    std::vector<std::vector<uint8_t>> pictures_;
    std::string name_ = "Simulated Camera";
    std::vector<UvcFrameSize> sizes_;
    detail::UvcFrameQueue frames_;
    std::mutex lock_;
    std::thread thread_;
    std::atomic<bool> running_{false};
    std::atomic<bool> gone_{false};
};

std::mutex s_lock;
std::shared_ptr<SimUvcDevice> s_device;

bool read_file(const std::filesystem::path& path, std::vector<uint8_t>* out) {
    std::ifstream file(path, std::ios::binary);
    if (!file) return false;
    out->assign(std::istreambuf_iterator<char>(file), std::istreambuf_iterator<char>());
    return out->size() >= 2 && (*out)[0] == 0xff && (*out)[1] == 0xd8;
}

std::vector<std::vector<uint8_t>> load_pictures(const std::filesystem::path& path) {
    std::vector<std::filesystem::path> files;
    std::error_code error;
    if (std::filesystem::is_directory(path, error)) {
        for (const auto& entry : std::filesystem::directory_iterator(path, error)) {
            const std::string extension = entry.path().extension().string();
            if (extension == ".jpg" || extension == ".jpeg") files.push_back(entry.path());
        }
        std::sort(files.begin(), files.end());
    } else {
        files.push_back(path);
    }
    std::vector<std::vector<uint8_t>> pictures;
    for (const auto& file : files) {
        std::vector<uint8_t> bytes;
        if (read_file(file, &bytes)) pictures.push_back(std::move(bytes));
    }
    return pictures;
}

bool cmd_attach(int argc, const char* const* argv, void*) {
    const char* fallback = getenv("SIMULATOR_USBH_UVC_PATH");
    const char* path = argc > 1 && strcmp(argv[1], "-") != 0 ? argv[1] : fallback;
    if (!path) {
        harness_reply("ERR %s: no path and SIMULATOR_USBH_UVC_PATH unset", argv[0]);
        return true;
    }
    std::vector<UvcFrameSize> sizes;
    const char* list = argc > 2 ? argv[2] : "1280x720";
    while (*list) {
        unsigned width = 0;
        unsigned height = 0;
        int used = 0;
        if (sscanf(list, "%ux%u%n", &width, &height, &used) != 2) {
            harness_reply("ERR %s: sizes must be WxH[,WxH...]", argv[0]);
            return true;
        }
        UvcFrameSize size;
        size.width = static_cast<uint16_t>(width);
        size.height = static_cast<uint16_t>(height);
        size.intervals.assign(std::begin(kIntervals), std::end(kIntervals));
        sizes.push_back(std::move(size));
        list += used;
        if (*list == ',') list++;
    }
    auto pictures = load_pictures(path);
    if (pictures.empty()) {
        harness_reply("ERR %s: no JPEG at %s", argv[0], path);
        return true;
    }
    std::shared_ptr<SimUvcDevice> device;
    {
        std::lock_guard<std::mutex> guard(s_lock);
        if (s_device) {
            harness_reply("ERR %s: a camera is attached", argv[0]);
            return true;
        }
        s_device = std::make_shared<SimUvcDevice>(std::move(pictures), std::move(sizes));
        device = s_device;
    }
    if (detail::callbacks().uvc_connected) detail::callbacks().uvc_connected(device);
    return true;
}

bool cmd_detach(int, const char* const*, void*) {
    std::shared_ptr<SimUvcDevice> device;
    {
        std::lock_guard<std::mutex> guard(s_lock);
        device = std::move(s_device);
    }
    if (!device) return true;
    device->mark_gone();
    if (detail::callbacks().uvc_disconnected) detail::callbacks().uvc_disconnected(device);
    return true;
}

}  // namespace

esp_err_t detail::uvc_install() {
    harness_register("usbh-uvc-attach", cmd_attach, nullptr);
    harness_register("usbh-uvc-detach", cmd_detach, nullptr);
    return ESP_OK;
}

}  // namespace usb_host
