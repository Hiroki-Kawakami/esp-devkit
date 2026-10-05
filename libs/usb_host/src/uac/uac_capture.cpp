/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "uac_capture.hpp"

#include <algorithm>
#include <cstring>
#include <new>

#include "esp_heap_caps.h"
#include "esp_log.h"
#include "host_internal.hpp"

namespace usb_host::detail {

namespace {

const char* TAG = "usb_host_uac";

constexpr uint8_t kSetCur = 0x01;
constexpr uint8_t kSamplingFreqControl = 0x01;

constexpr uint8_t kClassEndpointOut = 0 | kReqTypeClass |
                                      kReqRecipEndpoint;
constexpr uint8_t kStandardInterfaceOut = 0 | kReqTypeStandard |
                                          kReqRecipInterface;

constexpr uint32_t kControlTimeoutMs = 1000;
constexpr size_t kControlBytes = 16;
constexpr uint32_t kRingMs = 200;
constexpr uint32_t kTransferUs = 8000;
constexpr int kTransfers = 4;

struct Quirk {
    uint16_t vendor;
    uint16_t product;
    uint8_t declared_channels;
    uint32_t declared_rate;
    uint8_t channels;
    uint32_t rate;
};

// Declares 96 kHz mono and sends 48 kHz stereo: the byte rate matches, so the
// device is asked for what it declares and read as what it sends.
constexpr Quirk kQuirks[] = {
    {0x534d, 0x2109, 1, 96000, 2, 48000},
};

}  // namespace

esp_err_t UacInputDevice::open(uint8_t address,
                               std::shared_ptr<UacInputDevice>* out) {
    auto* raw = new (std::nothrow) UacInputDevice();
    if (!raw) return ESP_ERR_NO_MEM;
    std::shared_ptr<UacInputDevice> device(raw);
    const esp_err_t err = device->setup(address);
    if (err != ESP_OK) return err;
    for (size_t i = 0; i < device->formats_.size(); i++) {
        const UacFormat& format = device->formats_[i];
        const UacStreamAlt& alt = device->topology_.alts[i];
        ESP_LOGI(TAG, "capture at %u: alt %u, %u ch, %u/%u bit, %u rates (%u..%u), ep %02x/%u%s",
                 address, alt.alternate, format.channels, format.bit_resolution,
                 format.subframe_bytes * 8, static_cast<unsigned>(format.rates.size()),
                 static_cast<unsigned>(format.rates.empty() ? format.min_rate : format.rates.front()),
                 static_cast<unsigned>(format.rates.empty() ? format.max_rate : format.rates.back()),
                 alt.endpoint, alt.max_packet_bytes,
                 device->declared_rates_[i] ? " (quirk)" : "");
    }
    *out = std::move(device);
    return ESP_OK;
}

UacInputDevice::~UacInputDevice() {
    {
        std::lock_guard<std::mutex> guard(lock_);
        close_locked();
    }
    if (ctrl_) transfer_free(ctrl_);
    if (data_) vSemaphoreDelete(data_);
    if (xfer_.device()) close_device(xfer_.device());
}

esp_err_t UacInputDevice::setup(uint8_t address) {
    data_ = xSemaphoreCreateBinary();
    if (!data_) return ESP_ERR_NO_MEM;
    esp_err_t err = xfer_.init();
    if (err == ESP_OK) err = stream_.init();
    if (err != ESP_OK) return err;

    Device* handle = nullptr;
    err = open_device(address, &handle);
    if (err != ESP_OK) return err;
    xfer_.set_device(handle);

    err = uac_parse(config_descriptor(handle), true, &topology_);
    if (err != ESP_OK) return err;
    for (const UacStreamAlt& alt : topology_.alts) formats_.push_back(alt.format);
    declared_rates_.assign(formats_.size(), 0);

    const DeviceDesc* device = device_descriptor(handle);
    apply_quirks(device->idVendor, device->idProduct);
    speed_ = device_speed(handle);
    return transfer_alloc(sizeof(SetupPacket) + kControlBytes, 0,
                          MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL, &ctrl_);
}

void UacInputDevice::apply_quirks(uint16_t vendor, uint16_t product) {
    for (const Quirk& quirk : kQuirks) {
        if (quirk.vendor != vendor || quirk.product != product) continue;
        for (size_t i = 0; i < formats_.size(); i++) {
            UacFormat& format = formats_[i];
            if (format.channels != quirk.declared_channels ||
                !format.supports(quirk.declared_rate)) {
                continue;
            }
            format.channels = quirk.channels;
            format.rates = {quirk.rate};
            format.min_rate = 0;
            format.max_rate = 0;
            declared_rates_[i] = quirk.declared_rate;
        }
    }
}

esp_err_t UacInputDevice::control_out(uint8_t request_type, uint8_t request, uint16_t value,
                                      uint16_t index, const void* data, uint16_t length) {
    if (length) memcpy(ctrl_->data_buffer + sizeof(SetupPacket), data, length);
    return xfer_.control(ctrl_, request_type, request, value, index, length, kControlTimeoutMs);
}

esp_err_t UacInputDevice::open(size_t format, uint32_t rate) {
    std::lock_guard<std::mutex> guard(lock_);
    if (gone_) return ESP_ERR_NOT_FOUND;
    if (format >= formats_.size()) return ESP_ERR_INVALID_ARG;
    if (!formats_[format].supports(rate)) return ESP_ERR_NOT_SUPPORTED;
    close_locked();

    const UacStreamAlt& alt = topology_.alts[format];
    const uint32_t device_rate = declared_rates_[format] ? declared_rates_[format] : rate;
    const uint32_t interval = 1u << std::min<uint8_t>(alt.interval - 1, 15);
    const uint32_t period_us = (speed_ == Speed::High ? 125 : 1000) * interval;
    InStreamConfig config;
    config.endpoint = alt.endpoint;
    config.isochronous = true;
    config.max_packet_bytes = alt.max_packet_bytes;
    config.packets = std::min<int>(std::max<uint32_t>(1, kTransferUs / period_us),
                                   isoc_slots(speed_) / (kTransfers * static_cast<int>(interval)));
    config.transfers = kTransfers;
    if (config.packets < 1) return ESP_ERR_NOT_SUPPORTED;

    frame_bytes_ = static_cast<size_t>(formats_[format].channels) * alt.format.subframe_bytes;
    ring_bytes_ = (static_cast<size_t>(rate) * kRingMs / 1000) * frame_bytes_;
    ring_ = static_cast<uint8_t*>(heap_caps_malloc(ring_bytes_, MALLOC_CAP_SPIRAM));
    if (!ring_) return ESP_ERR_NO_MEM;
    read_ = 0;
    written_ = 0;
    xSemaphoreTake(data_, 0);

    esp_err_t err = interface_claim(xfer_.device(), alt.interface, alt.alternate);
    if (err != ESP_OK) {
        close_locked();
        return err;
    }
    active_ = static_cast<int>(format);
    err = control_out(kStandardInterfaceOut, kReqSetInterface, alt.alternate,
                      alt.interface, nullptr, 0);
    if (err == ESP_OK && alt.rate_control) {
        const uint8_t frequency[3] = {static_cast<uint8_t>(device_rate),
                                      static_cast<uint8_t>(device_rate >> 8),
                                      static_cast<uint8_t>(device_rate >> 16)};
        const esp_err_t rate_err = control_out(kClassEndpointOut, kSetCur,
                                               kSamplingFreqControl << 8, alt.endpoint,
                                               frequency, sizeof(frequency));
        if (rate_err != ESP_OK) {
            ESP_LOGW(TAG, "set %u Hz: %s", static_cast<unsigned>(device_rate),
                     esp_err_to_name(rate_err));
        }
    }
    if (err == ESP_OK) err = stream_.start(xfer_.device(), config, on_data, this);
    if (err != ESP_OK) close_locked();
    return err;
}

void UacInputDevice::close() {
    std::lock_guard<std::mutex> guard(lock_);
    close_locked();
}

void UacInputDevice::close_locked() {
    stream_.stop();
    if (active_ >= 0) {
        const UacStreamAlt& alt = topology_.alts[active_];
        if (!gone_) {
            control_out(kStandardInterfaceOut, kReqSetInterface, 0, alt.interface,
                        nullptr, 0);
        }
        interface_release(xfer_.device(), alt.interface);
        active_ = -1;
    }
    heap_caps_free(ring_);
    ring_ = nullptr;
    ring_bytes_ = 0;
}

void UacInputDevice::on_data(void* context, const uint8_t* data, size_t len, size_t, bool ok) {
    auto* self = static_cast<UacInputDevice*>(context);
    if (!ok || !len) return;
    const size_t written = self->written_.load(std::memory_order_relaxed);
    const size_t space =
        self->ring_bytes_ - (written - self->read_.load(std::memory_order_acquire));
    size_t take = std::min(len, space);
    take -= take % self->frame_bytes_;
    if (!take) return;
    const size_t at = written % self->ring_bytes_;
    const size_t first = std::min(take, self->ring_bytes_ - at);
    memcpy(self->ring_ + at, data, first);
    memcpy(self->ring_, data + first, take - first);
    self->written_.store(written + take, std::memory_order_release);
    xSemaphoreGive(self->data_);
}

esp_err_t UacInputDevice::read(void* data, size_t len, size_t* read, uint32_t timeout_ms) {
    *read = 0;
    if (gone_) return ESP_ERR_NOT_FOUND;
    if (!ring_) return ESP_ERR_INVALID_STATE;
    const TickType_t deadline = xTaskGetTickCount() + pdMS_TO_TICKS(timeout_ms);
    while (available() < frame_bytes_) {
        const TickType_t now = xTaskGetTickCount();
        if (static_cast<int32_t>(deadline - now) <= 0) return ESP_ERR_TIMEOUT;
        xSemaphoreTake(data_, deadline - now);
        if (gone_) return ESP_ERR_NOT_FOUND;
    }
    const size_t from = read_.load(std::memory_order_relaxed);
    size_t take = std::min(len, available());
    take -= take % frame_bytes_;
    const size_t at = from % ring_bytes_;
    const size_t first = std::min(take, ring_bytes_ - at);
    auto* out = static_cast<uint8_t*>(data);
    memcpy(out, ring_ + at, first);
    memcpy(out + first, ring_, take - first);
    read_.store(from + take, std::memory_order_release);
    *read = take;
    return ESP_OK;
}

size_t UacInputDevice::available() const {
    return written_.load(std::memory_order_acquire) - read_.load(std::memory_order_relaxed);
}

void UacInputDevice::mark_gone() {
    gone_ = true;
    xSemaphoreGive(data_);
}

}  // namespace usb_host::detail
