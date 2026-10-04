/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <atomic>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <mutex>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#include "esp_log.h"
#include "harness.h"
#include "host_internal.hpp"
#include "usb_host_uac.hpp"

namespace usb_host {

namespace {

const char* TAG = "usb_host_uac";

using Clock = std::chrono::steady_clock;

constexpr uint32_t kRingMs = 40;
constexpr float kVolumeMinDb = -60.0f;
constexpr float kVolumeMaxDb = 6.0f;

void put16(uint8_t* out, uint16_t value) {
    out[0] = static_cast<uint8_t>(value);
    out[1] = static_cast<uint8_t>(value >> 8);
}

void put32(uint8_t* out, uint32_t value) {
    put16(out, static_cast<uint16_t>(value));
    put16(out + 2, static_cast<uint16_t>(value >> 16));
}

class SimUacDevice final : public UacDevice {
public:
    SimUacDevice(std::string path, std::vector<uint32_t> rates) : path_(std::move(path)) {
        for (uint8_t bytes : {2, 3}) {
            UacFormat format;
            format.channels = 2;
            format.subframe_bytes = bytes;
            format.bit_resolution = bytes * 8;
            format.rates = rates;
            formats_.push_back(std::move(format));
        }
    }
    ~SimUacDevice() override { close(); }

    bool connected() const override { return !gone_; }
    const std::vector<UacFormat>& formats() const override { return formats_; }

    esp_err_t open(size_t format, uint32_t rate) override {
        std::lock_guard<std::mutex> guard(lock_);
        if (gone_) return ESP_ERR_NOT_FOUND;
        if (format >= formats_.size()) return ESP_ERR_INVALID_ARG;
        if (!formats_[format].supports(rate)) return ESP_ERR_NOT_SUPPORTED;
        close_locked();
        file_ = fopen(path_.c_str(), "wb");
        if (!file_) return ESP_FAIL;
        const uint8_t header[44] = {};
        fwrite(header, 1, sizeof(header), file_);
        format_ = formats_[format];
        rate_ = rate;
        frame_bytes_ = format_.channels * format_.subframe_bytes;
        bytes_ = 0;
        start_ = Clock::now();
        ESP_LOGI(TAG, "sim open %u Hz %u bit x%u -> %s", static_cast<unsigned>(rate),
                 format_.bit_resolution, format_.channels, path_.c_str());
        return ESP_OK;
    }

    void close() override {
        std::lock_guard<std::mutex> guard(lock_);
        close_locked();
    }

    esp_err_t write(const void* data, size_t len) override {
        {
            std::lock_guard<std::mutex> guard(lock_);
            if (gone_) return ESP_ERR_NOT_FOUND;
            if (!file_) return ESP_ERR_INVALID_STATE;
            fwrite(data, 1, len, file_);
            bytes_ += len;
        }
        const uint64_t ahead_frames = static_cast<uint64_t>(rate_) * kRingMs / 1000;
        const uint64_t frames = bytes_ / frame_bytes_;
        if (frames > ahead_frames) sleep_until_frame(frames - ahead_frames);
        return gone_ ? ESP_ERR_NOT_FOUND : ESP_OK;
    }

    void drain() override {
        if (frame_bytes_) sleep_until_frame(bytes_ / frame_bytes_);
    }

    bool has_volume() const override { return true; }
    bool has_mute() const override { return true; }
    float volume_min_db() const override { return kVolumeMinDb; }
    float volume_max_db() const override { return kVolumeMaxDb; }

    void set_volume_db(float db) override {
        if (db < kVolumeMinDb) db = kVolumeMinDb;
        if (db > kVolumeMaxDb) db = kVolumeMaxDb;
        ESP_LOGI(TAG, "sim volume %.1f dB", db);
    }

    void set_mute(bool mute) override { ESP_LOGI(TAG, "sim mute %d", mute); }

    void mark_gone() { gone_ = true; }

private:
    void sleep_until_frame(uint64_t frame) {
        const auto due = start_ + std::chrono::microseconds(frame * 1000000 / rate_);
        while (!gone_ && Clock::now() < due) {
            std::this_thread::sleep_for(std::min<Clock::duration>(due - Clock::now(),
                                                                  std::chrono::milliseconds(10)));
        }
    }

    void close_locked() {
        if (!file_) return;
        uint8_t header[44];
        memcpy(header, "RIFF", 4);
        put32(header + 4, static_cast<uint32_t>(36 + bytes_));
        memcpy(header + 8, "WAVEfmt ", 8);
        put32(header + 16, 16);
        put16(header + 20, 1);
        put16(header + 22, format_.channels);
        put32(header + 24, rate_);
        put32(header + 28, static_cast<uint32_t>(rate_ * frame_bytes_));
        put16(header + 32, static_cast<uint16_t>(frame_bytes_));
        put16(header + 34, static_cast<uint16_t>(format_.subframe_bytes * 8));
        memcpy(header + 36, "data", 4);
        put32(header + 40, static_cast<uint32_t>(bytes_));
        fseek(file_, 0, SEEK_SET);
        fwrite(header, 1, sizeof(header), file_);
        fclose(file_);
        file_ = nullptr;
    }

    std::string path_;
    std::vector<UacFormat> formats_;
    std::mutex lock_;
    std::atomic<bool> gone_{false};
    FILE* file_ = nullptr;
    UacFormat format_;
    uint32_t rate_ = 0;
    size_t frame_bytes_ = 0;
    std::atomic<uint64_t> bytes_{0};
    Clock::time_point start_;
};

std::mutex s_lock;
std::shared_ptr<SimUacDevice> s_device;

std::vector<uint32_t> parse_rates(const char* text) {
    std::vector<uint32_t> rates;
    while (text && *text) {
        char* end = nullptr;
        const unsigned long rate = strtoul(text, &end, 10);
        if (end == text) break;
        if (rate) rates.push_back(static_cast<uint32_t>(rate));
        text = *end == ',' ? end + 1 : end;
    }
    return rates;
}

bool cmd_attach(int argc, const char* const* argv, void*) {
    const char* path = argc > 1 ? argv[1] : "captures/usb_audio.wav";
    std::vector<uint32_t> rates = parse_rates(argc > 2 ? argv[2] : "44100,48000");
    if (rates.empty()) {
        harness_reply("ERR %s: no sample rates", argv[0]);
        return true;
    }
    std::shared_ptr<SimUacDevice> device;
    {
        std::lock_guard<std::mutex> guard(s_lock);
        if (s_device) {
            harness_reply("ERR %s: a device is attached", argv[0]);
            return true;
        }
        s_device = std::make_shared<SimUacDevice>(path, std::move(rates));
        device = s_device;
    }
    if (detail::callbacks().uac_connected) detail::callbacks().uac_connected(device);
    return true;
}

bool cmd_detach(int, const char* const*, void*) {
    std::shared_ptr<SimUacDevice> device;
    {
        std::lock_guard<std::mutex> guard(s_lock);
        device = std::move(s_device);
    }
    if (!device) return true;
    device->mark_gone();
    if (detail::callbacks().uac_disconnected) detail::callbacks().uac_disconnected(device);
    return true;
}

}  // namespace

esp_err_t detail::uac_install() {
    harness_register("usbh-uac-attach", cmd_attach, nullptr);
    harness_register("usbh-uac-detach", cmd_detach, nullptr);
    return ESP_OK;
}

}  // namespace usb_host
