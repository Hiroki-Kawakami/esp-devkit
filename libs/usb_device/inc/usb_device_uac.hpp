/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <atomic>
#include <functional>
#include <string>
#include <vector>

#include "usb_device.hpp"

namespace usb_device {

// A USB Audio Class 2.0 speaker: an isochronous OUT stream with a feature
// unit for mute and volume. Alternate 0 of the streaming interface is idle,
// each of `formats` is one alternate from 1 on.
class UacSpeaker : public Function {
public:
    // Async plays on the device's clock and tells the host the rate through
    // a feedback endpoint (set_feedback); adaptive follows the host's.
    enum class Sync : uint8_t { Async, Adaptive };

    struct Format {
        uint8_t channels;
        uint8_t bits;
    };

    struct Config {
        std::string name;
        Sync sync = Sync::Async;
        std::vector<Format> formats;
        std::vector<uint32_t> sample_rates;
        // 1/256 dB.
        int16_t volume_min = -40 * 256;
        int16_t volume_max = 0;
        int16_t volume_resolution = 256;
        int16_t volume = 0;
        // bInterval of the data and feedback endpoints at high speed; full
        // speed uses every frame.
        uint8_t data_interval = 4;
        uint8_t feedback_interval = 4;

        // These run on the device task. `started` also follows a sample rate
        // change while streaming; `received` is one packet of interleaved PCM.
        std::function<void(const Format& format, uint32_t sample_rate)> started;
        std::function<void()> stopped;
        std::function<void(const uint8_t* pcm, size_t bytes)> received;
        std::function<void(bool mute, int16_t volume)> volume_changed;
    };

    explicit UacSpeaker(Config config);
    ~UacSpeaker() override;

    // The rate the device consumes samples at, which the feedback endpoint
    // reports. From any task.
    void set_feedback(double samples_per_second);

    bool muted() const { return mute_; }
    int16_t volume() const { return volume_; }

    void describe(ConfigBuilder& config) override;
    void configured() override;
    void unconfigured() override;
    bool set_alternate(uint8_t interface, uint8_t alternate) override;
    bool control(const ControlRequest& request, uint8_t* data, size_t* bytes) override;

private:
    static constexpr int kDataTransfers = 8;
    static constexpr int kFeedbackTransfers = 2;

    static void data_done(Transfer* transfer);
    static void feedback_done(Transfer* transfer);
    void submit_data(Transfer* transfer);
    void submit_feedback(Transfer* transfer);
    void stop();
    bool clock_request(const ControlRequest& request, uint8_t* data, size_t* bytes);
    bool feature_request(const ControlRequest& request, uint8_t* data, size_t* bytes);

    Config config_;
    uint8_t control_interface_ = 0;
    uint8_t stream_interface_ = 0;
    uint8_t data_address_ = 0;
    uint8_t feedback_address_ = 0;
    Speed speed_ = Speed::High;
    uint32_t buffer_bytes_ = 0;
    Transfer data_[kDataTransfers] = {};
    Transfer feedback_[kFeedbackTransfers] = {};
    bool data_busy_[kDataTransfers] = {};
    bool feedback_busy_[kFeedbackTransfers] = {};
    uint8_t alternate_ = 0;
    uint32_t sample_rate_ = 0;
    bool mute_ = false;
    int16_t volume_ = 0;
    std::atomic<uint32_t> feedback_value_ = 0;
};

}  // namespace usb_device
