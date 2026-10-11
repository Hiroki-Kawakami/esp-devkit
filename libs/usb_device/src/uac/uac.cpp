/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "usb_device_uac.hpp"

#include <algorithm>
#include <cmath>
#include <cstring>

#include "esp_heap_caps.h"
#include "esp_log.h"

namespace usb_device {

namespace {

const char* TAG = "usb_uac";

constexpr uint8_t kClassAudio = 0x01;
constexpr uint8_t kSubclassControl = 0x01;
constexpr uint8_t kSubclassStreaming = 0x02;
constexpr uint8_t kProtocolV2 = 0x20;
constexpr uint8_t kCsInterface = 0x24;
constexpr uint8_t kCsEndpoint = 0x25;

constexpr uint8_t kAcHeader = 0x01;
constexpr uint8_t kAcInputTerminal = 0x02;
constexpr uint8_t kAcOutputTerminal = 0x03;
constexpr uint8_t kAcFeatureUnit = 0x06;
constexpr uint8_t kAcClockSource = 0x0a;
constexpr uint8_t kAsGeneral = 0x01;
constexpr uint8_t kAsFormatType = 0x02;
constexpr uint8_t kEpGeneral = 0x01;

constexpr uint8_t kClockId = 1;
constexpr uint8_t kInputId = 2;
constexpr uint8_t kFeatureId = 3;
constexpr uint8_t kOutputId = 4;

constexpr uint8_t kCategorySpeaker = 0x01;
constexpr uint16_t kTerminalStreaming = 0x0101;
constexpr uint16_t kTerminalSpeaker = 0x0301;
constexpr uint8_t kFormatTypeI = 0x01;
constexpr uint32_t kFormatPcm = 0x01;

constexpr uint8_t kSyncAsync = 0x04;
constexpr uint8_t kSyncAdaptive = 0x08;
constexpr uint8_t kUsageFeedback = 0x10;

constexpr uint8_t kRequestCur = 0x01;
constexpr uint8_t kRequestRange = 0x02;
constexpr uint8_t kClockFrequency = 0x01;
constexpr uint8_t kClockValid = 0x02;
constexpr uint8_t kFeatureMute = 0x01;
constexpr uint8_t kFeatureVolume = 0x02;

constexpr uint8_t kRequestDirIn = 0x80;
constexpr uint8_t kRequestTypeMask = 0x60;
constexpr uint8_t kRequestTypeClass = 0x20;

constexpr uint16_t kHighSpeedFeedbackBytes = 4;
constexpr uint16_t kFullSpeedFeedbackBytes = 3;
constexpr uint32_t kBufferAlignment = 128;

void put16(std::vector<uint8_t>& out, uint16_t value) {
    out.push_back(static_cast<uint8_t>(value));
    out.push_back(static_cast<uint8_t>(value >> 8));
}

void put32(std::vector<uint8_t>& out, uint32_t value) {
    put16(out, static_cast<uint16_t>(value));
    put16(out, static_cast<uint16_t>(value >> 16));
}

uint8_t subslot(const UacSpeaker::Format& format) {
    return static_cast<uint8_t>((format.bits + 7) / 8);
}

uint32_t channel_config(uint8_t channels) {
    return channels == 2 ? 0x3 : 0;
}

uint32_t interval_us(Speed speed, uint8_t interval) {
    return (speed == Speed::High ? 125u : 1000u) << (std::max<uint8_t>(interval, 1) - 1);
}

// One more frame than the rate needs, for the clocks to drift into.
uint16_t data_packet_bytes(Speed speed, uint8_t interval, uint32_t rate,
                           const UacSpeaker::Format& format) {
    const uint64_t frames = (static_cast<uint64_t>(rate) * interval_us(speed, interval) +
                             999999) / 1000000 + 1;
    return static_cast<uint16_t>(frames * format.channels * subslot(format));
}

}  // namespace

UacSpeaker::UacSpeaker(Config config) : config_(std::move(config)) {
    mute_ = false;
    volume_ = std::clamp(config_.volume, config_.volume_min, config_.volume_max);
    sample_rate_ = config_.sample_rates.empty() ? 0 : config_.sample_rates.front();
    set_feedback(sample_rate_);

    const uint32_t rate = config_.sample_rates.empty()
        ? 0 : *std::max_element(config_.sample_rates.begin(), config_.sample_rates.end());
    uint32_t largest = 0;
    for (const Format& format : config_.formats) {
        largest = std::max<uint32_t>(largest, data_packet_bytes(Speed::High, config_.data_interval,
                                                                rate, format));
        largest = std::max<uint32_t>(largest, data_packet_bytes(Speed::Full, 1, rate, format));
    }
    buffer_bytes_ = (largest + kBufferAlignment - 1) / kBufferAlignment * kBufferAlignment;
    for (Transfer& transfer : data_) {
        transfer.buffer = static_cast<uint8_t*>(heap_caps_aligned_alloc(
            kBufferAlignment, buffer_bytes_, MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL));
        transfer.length = buffer_bytes_;
        transfer.callback = data_done;
        transfer.context = this;
    }
    for (Transfer& transfer : feedback_) {
        transfer.buffer = static_cast<uint8_t*>(
            heap_caps_aligned_alloc(4, 4, MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL));
        transfer.callback = feedback_done;
        transfer.context = this;
    }
}

UacSpeaker::~UacSpeaker() {
    for (Transfer& transfer : data_) heap_caps_free(transfer.buffer);
    for (Transfer& transfer : feedback_) heap_caps_free(transfer.buffer);
}

void UacSpeaker::set_feedback(double samples_per_second) {
    // 16.16 samples per microframe at high speed, 10.14 per frame at full.
    const double value = speed_ == Speed::High ? samples_per_second / 8000.0 * 65536.0
                                               : samples_per_second / 1000.0 * 16384.0;
    feedback_value_ = static_cast<uint32_t>(std::lround(std::max(value, 0.0)));
}

void UacSpeaker::describe(ConfigBuilder& config) {
    const Speed speed = config.speed();
    const uint8_t data_interval = speed == Speed::High ? config_.data_interval : 1;
    const uint8_t feedback_interval = speed == Speed::High ? config_.feedback_interval : 1;
    uint8_t channels = 0;
    for (const Format& format : config_.formats) channels = std::max(channels, format.channels);

    config.association(kClassAudio, 0, kProtocolV2);
    control_interface_ = config.interface(kClassAudio, kSubclassControl, kProtocolV2, config_.name);

    const bool programmable = config_.sample_rates.size() > 1;
    std::vector<uint8_t> units;
    units.insert(units.end(), {8, kCsInterface, kAcClockSource, kClockId,
                               static_cast<uint8_t>(programmable ? 0x03 : 0x01),
                               static_cast<uint8_t>(programmable ? 0x07 : 0x05), 0, 0});
    units.insert(units.end(), {17, kCsInterface, kAcInputTerminal, kInputId});
    put16(units, kTerminalStreaming);
    units.insert(units.end(), {0, kClockId, channels});
    put32(units, channel_config(channels));
    units.insert(units.end(), {0, 0, 0, 0});
    units.insert(units.end(), {static_cast<uint8_t>(6 + 4 * (channels + 1)), kCsInterface,
                               kAcFeatureUnit, kFeatureId, kInputId});
    put32(units, 0x0000000f);
    for (uint8_t i = 0; i < channels; i++) put32(units, 0);
    units.push_back(0);
    units.insert(units.end(), {12, kCsInterface, kAcOutputTerminal, kOutputId});
    put16(units, kTerminalSpeaker);
    units.insert(units.end(), {0, kFeatureId, kClockId, 0, 0, 0});

    std::vector<uint8_t> header = {9, kCsInterface, kAcHeader};
    put16(header, 0x0200);
    header.push_back(kCategorySpeaker);
    put16(header, static_cast<uint16_t>(9 + units.size()));
    header.push_back(0);
    config.append(header.data(), header.size());
    config.append(units.data(), units.size());

    stream_interface_ = config.interface(kClassAudio, kSubclassStreaming, kProtocolV2);
    const uint32_t rate = config_.sample_rates.empty()
        ? 0 : *std::max_element(config_.sample_rates.begin(), config_.sample_rates.end());
    uint8_t data = 0;
    uint8_t feedback = 0;
    for (size_t i = 0; i < config_.formats.size(); i++) {
        const Format& format = config_.formats[i];
        config.alternate(stream_interface_, static_cast<uint8_t>(i + 1), kClassAudio,
                         kSubclassStreaming, kProtocolV2);
        std::vector<uint8_t> general = {16, kCsInterface, kAsGeneral, kInputId, 0, kFormatTypeI};
        put32(general, kFormatPcm);
        general.push_back(format.channels);
        put32(general, channel_config(format.channels));
        general.push_back(0);
        const uint8_t type[] = {6, kCsInterface, kAsFormatType, kFormatTypeI, subslot(format),
                                format.bits};
        config.append(general.data(), general.size());
        config.append(type, sizeof(type));

        const bool async = config_.sync == Sync::Async;
        data = config.endpoint(EndpointType::Isochronous, false,
                               data_packet_bytes(speed, data_interval, rate, format),
                               data_interval, async ? kSyncAsync : kSyncAdaptive, data);
        const uint8_t endpoint[] = {8, kCsEndpoint, kEpGeneral, 0, 0, 0, 0, 0};
        config.append(endpoint, sizeof(endpoint));
        if (async) {
            feedback = config.endpoint(
                EndpointType::Isochronous, true,
                speed == Speed::High ? kHighSpeedFeedbackBytes : kFullSpeedFeedbackBytes,
                feedback_interval, kUsageFeedback, feedback);
        }
    }
    data_address_ = data;
    feedback_address_ = feedback;
}

void UacSpeaker::configured() {
    speed_ = device()->speed();
    set_feedback(sample_rate_);
}

void UacSpeaker::unconfigured() {
    stop();
}

void UacSpeaker::stop() {
    if (!alternate_) return;
    alternate_ = 0;
    if (config_.stopped) config_.stopped();
}

bool UacSpeaker::set_alternate(uint8_t interface, uint8_t alternate) {
    if (interface == control_interface_) return alternate == 0;
    if (alternate > config_.formats.size()) return false;
    if (alternate == 0) {
        stop();
        return true;
    }
    alternate_ = alternate;
    if (config_.started) config_.started(config_.formats[alternate - 1], sample_rate_);
    for (int i = 0; i < kDataTransfers; i++) {
        if (!data_busy_[i]) submit_data(&data_[i]);
    }
    if (config_.sync == Sync::Async) {
        for (int i = 0; i < kFeedbackTransfers; i++) {
            if (!feedback_busy_[i]) submit_feedback(&feedback_[i]);
        }
    }
    return true;
}

void UacSpeaker::submit_data(Transfer* transfer) {
    const int index = static_cast<int>(transfer - data_);
    transfer->length = buffer_bytes_;
    data_busy_[index] = endpoint(data_address_).submit(transfer) == ESP_OK;
}

void UacSpeaker::submit_feedback(Transfer* transfer) {
    const int index = static_cast<int>(transfer - feedback_);
    const uint32_t value = feedback_value_;
    memcpy(transfer->buffer, &value, 4);
    transfer->length = speed_ == Speed::High ? kHighSpeedFeedbackBytes : kFullSpeedFeedbackBytes;
    feedback_busy_[index] = endpoint(feedback_address_).submit(transfer) == ESP_OK;
}

// A transfer canceled by an alternate change comes back after the new
// alternate is set, so whether to resubmit depends on the alternate now.
void UacSpeaker::data_done(Transfer* transfer) {
    auto* self = static_cast<UacSpeaker*>(transfer->context);
    self->data_busy_[transfer - self->data_] = false;
    if (!self->alternate_) return;
    if (transfer->status == TransferStatus::Completed && transfer->actual &&
        self->config_.received) {
        self->config_.received(transfer->buffer, transfer->actual);
    }
    self->submit_data(transfer);
}

void UacSpeaker::feedback_done(Transfer* transfer) {
    auto* self = static_cast<UacSpeaker*>(transfer->context);
    self->feedback_busy_[transfer - self->feedback_] = false;
    if (!self->alternate_) return;
    self->submit_feedback(transfer);
}

bool UacSpeaker::control(const ControlRequest& request, uint8_t* data, size_t* bytes) {
    if ((request.request_type & kRequestTypeMask) != kRequestTypeClass) return false;
    if ((request.index & 0xff) != control_interface_) return false;
    switch (request.index >> 8) {
        case kClockId: return clock_request(request, data, bytes);
        case kFeatureId: return feature_request(request, data, bytes);
        default: return false;
    }
}

bool UacSpeaker::clock_request(const ControlRequest& request, uint8_t* data, size_t* bytes) {
    const bool in = request.request_type & kRequestDirIn;
    const uint8_t selector = request.value >> 8;
    std::vector<uint8_t> reply;
    if (selector == kClockValid && in && request.request == kRequestCur) {
        reply.push_back(1);
    } else if (selector == kClockFrequency && request.request == kRequestCur) {
        if (!in) {
            uint32_t rate = 0;
            memcpy(&rate, data, std::min<size_t>(*bytes, 4));
            if (std::find(config_.sample_rates.begin(), config_.sample_rates.end(), rate) ==
                config_.sample_rates.end()) {
                ESP_LOGW(TAG, "%u Hz not offered", (unsigned)rate);
                return false;
            }
            sample_rate_ = rate;
            set_feedback(rate);
            if (alternate_ && config_.started) {
                config_.started(config_.formats[alternate_ - 1], rate);
            }
            return true;
        }
        put32(reply, sample_rate_);
    } else if (selector == kClockFrequency && in && request.request == kRequestRange) {
        put16(reply, static_cast<uint16_t>(config_.sample_rates.size()));
        for (uint32_t rate : config_.sample_rates) {
            put32(reply, rate);
            put32(reply, rate);
            put32(reply, 0);
        }
    } else {
        return false;
    }
    *bytes = std::min(*bytes, reply.size());
    memcpy(data, reply.data(), *bytes);
    return true;
}

bool UacSpeaker::feature_request(const ControlRequest& request, uint8_t* data, size_t* bytes) {
    const bool in = request.request_type & kRequestDirIn;
    const uint8_t selector = request.value >> 8;
    const uint8_t channel = request.value & 0xff;
    if (channel != 0) return false;
    std::vector<uint8_t> reply;
    if (selector == kFeatureMute && request.request == kRequestCur) {
        if (!in) {
            mute_ = *bytes > 0 && data[0];
            if (config_.volume_changed) config_.volume_changed(mute_, volume_);
            return true;
        }
        reply.push_back(mute_ ? 1 : 0);
    } else if (selector == kFeatureVolume && request.request == kRequestCur) {
        if (!in) {
            int16_t volume = 0;
            memcpy(&volume, data, std::min<size_t>(*bytes, 2));
            volume_ = std::clamp(volume, config_.volume_min, config_.volume_max);
            if (config_.volume_changed) config_.volume_changed(mute_, volume_);
            return true;
        }
        put16(reply, static_cast<uint16_t>(volume_));
    } else if (selector == kFeatureVolume && in && request.request == kRequestRange) {
        put16(reply, 1);
        put16(reply, static_cast<uint16_t>(config_.volume_min));
        put16(reply, static_cast<uint16_t>(config_.volume_max));
        put16(reply, static_cast<uint16_t>(config_.volume_resolution));
    } else {
        return false;
    }
    *bytes = std::min(*bytes, reply.size());
    memcpy(data, reply.data(), *bytes);
    return true;
}

}  // namespace usb_device
