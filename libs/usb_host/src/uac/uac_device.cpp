/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "uac_device.hpp"

#include <algorithm>
#include <cmath>
#include <cstring>
#include <new>

#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "host_internal.hpp"

namespace usb_host::detail {

namespace {

const char* TAG = "usb_host_uac";

constexpr uint8_t kSetCur = 0x01;
constexpr uint8_t kGetMin = 0x82;
constexpr uint8_t kGetMax = 0x83;
constexpr uint8_t kGetRes = 0x84;
constexpr uint8_t kSamplingFreqControl = 0x01;
constexpr uint8_t kMuteControl = 0x01;
constexpr uint8_t kVolumeControl = 0x02;

constexpr uint8_t kClassInterfaceOut = 0 | kReqTypeClass |
                                       kReqRecipInterface;
constexpr uint8_t kClassInterfaceIn = kReqDirIn |
                                      kReqTypeClass |
                                      kReqRecipInterface;
constexpr uint8_t kClassEndpointOut = 0 | kReqTypeClass |
                                      kReqRecipEndpoint;
constexpr uint8_t kStandardInterfaceOut = 0 | kReqTypeStandard |
                                          kReqRecipInterface;

constexpr uint32_t kControlTimeoutMs = 1000;
constexpr size_t kControlBytes = 64;
constexpr uint32_t kFeatureDrainMs = 1000;

uint8_t control_channels(uint8_t channels) {
    return (channels & 1) ? 1 : static_cast<uint8_t>(channels & ~1);
}

int lowest_channel(uint8_t channels) {
    return __builtin_ctz(channels);
}

}  // namespace

esp_err_t UacOutputDevice::open(uint8_t address,
                                std::shared_ptr<UacOutputDevice>* out) {
    auto* raw = new (std::nothrow) UacOutputDevice();
    if (!raw) return ESP_ERR_NO_MEM;
    std::shared_ptr<UacOutputDevice> device(raw);
    const esp_err_t err = device->setup(address);
    if (err != ESP_OK) return err;
    for (const UacStreamAlt& alt : device->topology_.alts) {
        const UacFormat& format = alt.format;
        ESP_LOGI(TAG, "audio at %u: alt %u, %u ch, %u/%u bit, %u rates (%u..%u), ep %02x/%u",
                 address, alt.alternate, format.channels, format.bit_resolution,
                 format.subframe_bytes * 8, static_cast<unsigned>(format.rates.size()),
                 static_cast<unsigned>(format.rates.empty() ? format.min_rate : format.rates.front()),
                 static_cast<unsigned>(format.rates.empty() ? format.max_rate : format.rates.back()),
                 alt.endpoint, alt.max_packet_bytes);
    }
    if (device->has_volume()) {
        ESP_LOGI(TAG, "volume %.1f..%.1f dB, step %.2f dB", device->volume_min_ / 256.0f,
                 device->volume_max_ / 256.0f, device->volume_res_ / 256.0f);
    }
    *out = std::move(device);
    return ESP_OK;
}

UacOutputDevice::~UacOutputDevice() {
    {
        std::lock_guard<std::mutex> guard(lock_);
        close_locked();
    }
    {
        std::lock_guard<std::mutex> guard(feature_lock_);
        closing_ = true;
    }
    const TickType_t deadline = xTaskGetTickCount() + pdMS_TO_TICKS(kFeatureDrainMs);
    while (true) {
        {
            std::lock_guard<std::mutex> guard(feature_lock_);
            if (!feature_busy_) break;
        }
        if (static_cast<int32_t>(xTaskGetTickCount() - deadline) >= 0) {
            ESP_LOGE(TAG, "feature unit request did not complete");
            break;
        }
        vTaskDelay(pdMS_TO_TICKS(1));
    }
    if (feature_) transfer_free(feature_);
    if (ctrl_) transfer_free(ctrl_);
    if (xfer_.device()) close_device(xfer_.device());
}

esp_err_t UacOutputDevice::setup(uint8_t address) {
    esp_err_t err = xfer_.init();
    if (err == ESP_OK) err = stream_.init();
    if (err != ESP_OK) return err;

    Device* handle = nullptr;
    err = open_device(address, &handle);
    if (err != ESP_OK) return err;
    xfer_.set_device(handle);

    err = uac_parse(config_descriptor(handle), false, &topology_);
    if (err != ESP_OK) return err;
    for (const UacStreamAlt& alt : topology_.alts) formats_.push_back(alt.format);

    speed_ = device_speed(handle);

    err = transfer_alloc(kControlBytes, 0, MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL, &ctrl_);
    if (err == ESP_OK) {
        err = transfer_alloc(kControlBytes, 0, MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL, &feature_);
    }
    if (err != ESP_OK) return err;
    feature_->device = handle;
    feature_->bEndpointAddress = 0;
    feature_->callback = feature_done;
    feature_->context = this;

    mute_channels_ = control_channels(topology_.feature.mute_channels);
    volume_channels_ = control_channels(topology_.feature.volume_channels);
    if (volume_channels_) probe_volume();
    return ESP_OK;
}

uint16_t UacOutputDevice::feature_index() const {
    return static_cast<uint16_t>((topology_.feature.id << 8) | topology_.control_interface);
}

void UacOutputDevice::probe_volume() {
    const uint16_t value =
        static_cast<uint16_t>((kVolumeControl << 8) | lowest_channel(volume_channels_));
    int16_t min = 0;
    int16_t max = 0;
    int16_t res = 0;
    if (control_in(kClassInterfaceIn, kGetMin, value, feature_index(), &min, 2) != ESP_OK ||
        control_in(kClassInterfaceIn, kGetMax, value, feature_index(), &max, 2) != ESP_OK ||
        min >= max) {
        ESP_LOGW(TAG, "feature unit %u: no usable volume range", topology_.feature.id);
        volume_channels_ = 0;
        return;
    }
    if (control_in(kClassInterfaceIn, kGetRes, value, feature_index(), &res, 2) != ESP_OK ||
        res <= 0) {
        res = 1;
    }
    volume_min_ = min;
    volume_max_ = max;
    volume_res_ = res;
}

esp_err_t UacOutputDevice::control_out(uint8_t request_type, uint8_t request, uint16_t value,
                                       uint16_t index, const void* data, uint16_t length) {
    if (length) memcpy(ctrl_->data_buffer + sizeof(SetupPacket), data, length);
    return xfer_.control(ctrl_, request_type, request, value, index, length, kControlTimeoutMs);
}

esp_err_t UacOutputDevice::control_in(uint8_t request_type, uint8_t request, uint16_t value,
                                      uint16_t index, void* data, uint16_t length) {
    const esp_err_t err =
        xfer_.control(ctrl_, request_type, request, value, index, length, kControlTimeoutMs);
    if (err != ESP_OK) return err;
    if (ctrl_->actual_num_bytes < static_cast<int>(sizeof(SetupPacket) + length)) {
        return ESP_ERR_INVALID_RESPONSE;
    }
    memcpy(data, ctrl_->data_buffer + sizeof(SetupPacket), length);
    return ESP_OK;
}

esp_err_t UacOutputDevice::open(size_t format, uint32_t rate) {
    std::lock_guard<std::mutex> guard(lock_);
    if (gone_) return ESP_ERR_NOT_FOUND;
    if (format >= topology_.alts.size()) return ESP_ERR_INVALID_ARG;
    const UacStreamAlt& alt = topology_.alts[format];
    if (!alt.format.supports(rate)) return ESP_ERR_NOT_SUPPORTED;
    close_locked();

    const uint32_t interval = 1u << std::min<uint8_t>(alt.interval - 1, 15);
    IsocOutConfig config;
    config.endpoint = alt.endpoint;
    config.max_packet_bytes = alt.max_packet_bytes;
    config.period_us = (speed_ == Speed::High ? 125 : 1000) * interval;
    config.interval = interval;
    config.speed = speed_;
    config.rate = rate;
    config.frame_bytes = static_cast<size_t>(alt.format.channels) * alt.format.subframe_bytes;

    esp_err_t err = interface_claim(xfer_.device(), alt.interface,
                                             alt.alternate);
    if (err != ESP_OK) return err;
    active_ = static_cast<int>(format);
    err = control_out(kStandardInterfaceOut, kReqSetInterface, alt.alternate,
                      alt.interface, nullptr, 0);
    if (err != ESP_OK) {
        close_locked();
        return err;
    }
    if (alt.rate_control) {
        const uint8_t frequency[3] = {static_cast<uint8_t>(rate), static_cast<uint8_t>(rate >> 8),
                                      static_cast<uint8_t>(rate >> 16)};
        err = control_out(kClassEndpointOut, kSetCur, kSamplingFreqControl << 8, alt.endpoint,
                          frequency, sizeof(frequency));
        if (err != ESP_OK) ESP_LOGW(TAG, "set %u Hz: %s", static_cast<unsigned>(rate), esp_err_to_name(err));
    }
    err = stream_.start(xfer_.device(), config);
    if (err != ESP_OK) {
        close_locked();
        return err;
    }

    std::lock_guard<std::mutex> feature_guard(feature_lock_);
    if (mute_known_) mute_pending_ = mute_channels_;
    if (volume_known_) volume_pending_ = volume_channels_;
    submit_feature_locked();
    return ESP_OK;
}

void UacOutputDevice::close() {
    std::lock_guard<std::mutex> guard(lock_);
    close_locked();
}

void UacOutputDevice::close_locked() {
    if (active_ < 0) return;
    stream_.stop();
    const UacStreamAlt& alt = topology_.alts[active_];
    if (!gone_) {
        control_out(kStandardInterfaceOut, kReqSetInterface, 0, alt.interface, nullptr,
                    0);
    }
    interface_release(xfer_.device(), alt.interface);
    active_ = -1;
}

esp_err_t UacOutputDevice::write(const void* data, size_t len) {
    if (gone_) return ESP_ERR_NOT_FOUND;
    return stream_.write(data, len);
}

void UacOutputDevice::drain() {
    stream_.drain();
}

void UacOutputDevice::mark_gone() {
    gone_ = true;
    stream_.abort();
}

void UacOutputDevice::set_volume_db(float db) {
    if (!has_volume()) return;
    float units = std::isfinite(db) ? db * 256.0f : (db < 0 ? volume_min_ : volume_max_);
    units = std::clamp(units, static_cast<float>(volume_min_), static_cast<float>(volume_max_));
    int32_t value = volume_min_ + std::lround((units - volume_min_) / volume_res_) * volume_res_;
    value = units >= volume_max_ ? volume_max_ : std::min<int32_t>(value, volume_max_);

    std::lock_guard<std::mutex> guard(feature_lock_);
    if (volume_known_ && volume_ == value) return;
    volume_ = static_cast<int16_t>(value);
    volume_known_ = true;
    volume_pending_ = volume_channels_;
    submit_feature_locked();
}

void UacOutputDevice::set_mute(bool mute) {
    if (!has_mute()) return;
    std::lock_guard<std::mutex> guard(feature_lock_);
    if (mute_known_ && mute_ == mute) return;
    mute_ = mute;
    mute_known_ = true;
    mute_pending_ = mute_channels_;
    submit_feature_locked();
}

void UacOutputDevice::submit_feature_locked() {
    if (feature_busy_ || closing_ || gone_) return;
    uint8_t control = 0;
    int channel = 0;
    uint16_t length = 0;
    uint8_t* data = feature_->data_buffer + sizeof(SetupPacket);
    if (mute_pending_) {
        channel = lowest_channel(mute_pending_);
        mute_pending_ &= ~(1 << channel);
        control = kMuteControl;
        data[0] = mute_;
        length = 1;
    } else if (volume_pending_) {
        channel = lowest_channel(volume_pending_);
        volume_pending_ &= ~(1 << channel);
        control = kVolumeControl;
        data[0] = static_cast<uint8_t>(volume_);
        data[1] = static_cast<uint8_t>(static_cast<uint16_t>(volume_) >> 8);
        length = 2;
    } else {
        return;
    }
    auto* setup = reinterpret_cast<SetupPacket*>(feature_->data_buffer);
    setup->bmRequestType = kClassInterfaceOut;
    setup->bRequest = kSetCur;
    setup->wValue = static_cast<uint16_t>((control << 8) | channel);
    setup->wIndex = feature_index();
    setup->wLength = length;
    feature_->num_bytes = static_cast<int>(sizeof(SetupPacket) + length);
    feature_busy_ = true;
    const esp_err_t err = transfer_submit_control(feature_);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "feature unit request: %s", esp_err_to_name(err));
        feature_busy_ = false;
    }
}

void UacOutputDevice::feature_done(Transfer* transfer) {
    auto* self = static_cast<UacOutputDevice*>(transfer->context);
    std::lock_guard<std::mutex> guard(self->feature_lock_);
    self->feature_busy_ = false;
    if (transfer->status != TransferStatus::Completed &&
        transfer->status != TransferStatus::NoDevice) {
        ESP_LOGW(TAG, "feature unit request: status %d", transfer->status);
    }
    self->submit_feature_locked();
}

}  // namespace usb_host::detail
