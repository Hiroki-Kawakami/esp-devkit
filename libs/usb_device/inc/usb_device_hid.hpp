/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <functional>
#include <string>
#include <vector>

#include "freertos/FreeRTOS.h"
#include "usb_device.hpp"

namespace usb_device {

struct __attribute__((packed)) KeyboardReport {
    uint8_t modifiers;
    uint8_t reserved;
    uint8_t keys[6];
};

struct __attribute__((packed)) MouseReport {
    uint8_t buttons;
    int8_t x;
    int8_t y;
    int8_t wheel;
};

// One HID interface with an interrupt IN endpoint. Output reports arrive as
// SET_REPORT on the control pipe.
class Hid : public Function {
public:
    // A boot interface takes the boot report layout: KeyboardReport or
    // MouseReport, without a report ID.
    enum class Boot : uint8_t { None = 0, Keyboard = 1, Mouse = 2 };

    struct Config {
        std::string name;
        Boot boot = Boot::None;
        // Empty takes the boot device's descriptor.
        std::vector<uint8_t> report_descriptor;
        // The longest input report, its report ID included; 0 takes the boot
        // report's size.
        uint16_t max_report_bytes = 0;
        // bInterval at high speed; full speed polls every frame.
        uint8_t interval = 4;

        // These run on the device task. `report_id` is 0 without report IDs,
        // and `data` starts with the ID otherwise.
        std::function<void()> configured;
        std::function<void()> unconfigured;
        std::function<void()> suspended;
        std::function<void()> resumed;
        std::function<void(uint8_t report_id, const uint8_t* data, size_t bytes)> output;
    };

    explicit Hid(Config config);
    ~Hid() override;

    // From any task; reports go out in order. ESP_ERR_INVALID_STATE while
    // unconfigured or suspended, ESP_ERR_NO_MEM when the queue is full.
    esp_err_t send(const void* report, size_t bytes);

    void describe(ConfigBuilder& config) override;
    void configured() override;
    void unconfigured() override;
    void suspended() override;
    void resumed() override;
    bool control(const ControlRequest& request, uint8_t* data, size_t* bytes) override;

private:
    static constexpr int kQueueReports = 16;
    static constexpr int kStoredReports = 8;

    static void done(Transfer* transfer);
    void set_active(bool active);
    void submit_head();
    void hid_descriptor(uint8_t* out) const;
    uint8_t* slot(int index) const { return buffer_ + index * slot_bytes_; }
    bool get_report(uint8_t id, uint8_t* data, size_t* bytes);

    Config config_;
    bool report_ids_ = false;
    uint8_t interface_ = 0;
    uint8_t in_address_ = 0;
    uint32_t slot_bytes_ = 0;
    uint8_t* buffer_ = nullptr;
    uint16_t lengths_[kQueueReports] = {};
    int head_ = 0;
    int count_ = 0;
    bool busy_ = false;
    bool active_ = false;
    Transfer transfer_ = {};
    uint8_t* stored_ = nullptr;
    uint8_t stored_ids_[kStoredReports] = {};
    uint16_t stored_lengths_[kStoredReports] = {};
    int stored_count_ = 0;
    uint8_t idle_ = 0;
    uint8_t protocol_ = 1;
    portMUX_TYPE lock_ = portMUX_INITIALIZER_UNLOCKED;
};

}  // namespace usb_device
