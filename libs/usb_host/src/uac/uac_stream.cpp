/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "uac_stream.hpp"

#include <algorithm>
#include <cstring>

#include "esp_heap_caps.h"
#include "esp_log.h"

namespace usb_host::detail {

namespace {

const char* TAG = "usb_host_uac";

constexpr uint32_t kRingMs = 40;
constexpr uint32_t kTransferUs = 8000;
constexpr uint32_t kWriteWaitMs = 50;
constexpr int kWriteStalls = 10;
constexpr uint32_t kStopTimeoutMs = 200;

}  // namespace

void PacketClock::start(uint32_t rate, uint32_t period_us) {
    rate_ = rate;
    period_us_ = period_us;
    carry_ = 0;
}

uint32_t PacketClock::next() {
    carry_ += static_cast<uint64_t>(rate_) * period_us_;
    const uint32_t frames = static_cast<uint32_t>(carry_ / 1000000);
    carry_ -= static_cast<uint64_t>(frames) * 1000000;
    return frames;
}

uint32_t PacketClock::max_frames() const {
    return static_cast<uint32_t>((static_cast<uint64_t>(rate_) * period_us_ + 999999) / 1000000);
}

IsocOutStream::~IsocOutStream() {
    stop();
    if (space_) vSemaphoreDelete(space_);
    if (idle_) vSemaphoreDelete(idle_);
}

esp_err_t IsocOutStream::init() {
    space_ = xSemaphoreCreateBinary();
    idle_ = xSemaphoreCreateBinary();
    return space_ && idle_ ? ESP_OK : ESP_ERR_NO_MEM;
}

esp_err_t IsocOutStream::start(Device* device, const IsocOutConfig& config) {
    stop();
    device_ = device;
    frame_bytes_ = config.frame_bytes;
    clock_.start(config.rate, config.period_us);
    const size_t packet_bytes = clock_.max_frames() * frame_bytes_;
    if (!frame_bytes_ || packet_bytes > config.max_packet_bytes) return ESP_ERR_NOT_SUPPORTED;

    packets_ = static_cast<int>(std::max<uint32_t>(1, kTransferUs / config.period_us));
    packets_ = std::min<int>(packets_, isoc_slots(config.speed) / (kTransfers * static_cast<int>(config.interval)));
    if (packets_ < 1) return ESP_ERR_NOT_SUPPORTED;
    transfer_ms_ = (packets_ * config.period_us + 999) / 1000;

    ring_bytes_ = (static_cast<size_t>(config.rate) * kRingMs / 1000 + 1) * frame_bytes_;
    ring_ = static_cast<uint8_t*>(heap_caps_malloc(ring_bytes_, MALLOC_CAP_SPIRAM));
    if (!ring_) ring_ = static_cast<uint8_t*>(heap_caps_malloc(ring_bytes_, MALLOC_CAP_DEFAULT));
    if (!ring_) return ESP_ERR_NO_MEM;
    read_ = 0;
    written_ = 0;

    for (Transfer*& transfer : transfers_) {
        const esp_err_t err = transfer_alloc(packets_ * packet_bytes, packets_,
                                             MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL, &transfer);
        if (err != ESP_OK) {
            free_buffers();
            return err;
        }
        transfer->device = device_;
        transfer->bEndpointAddress = config.endpoint;
        transfer->callback = done;
        transfer->context = this;
    }

    xSemaphoreTake(idle_, 0);
    stopping_ = false;
    aborted_ = false;
    running_ = true;
    for (Transfer* transfer : transfers_) fill(transfer);
    for (Transfer* transfer : transfers_) {
        inflight_++;
        const esp_err_t err = transfer_submit(transfer);
        if (err != ESP_OK) {
            ESP_LOGE(TAG, "isochronous submit: %s", esp_err_to_name(err));
            inflight_--;
            stop();
            return err;
        }
    }
    return ESP_OK;
}

void IsocOutStream::stop() {
    if (!running_) return;
    stopping_ = true;
    running_ = false;
    xSemaphoreGive(space_);
    if (inflight_ > 0 && xSemaphoreTake(idle_, pdMS_TO_TICKS(kStopTimeoutMs)) != pdTRUE) {
        const uint8_t endpoint = transfers_[0]->bEndpointAddress;
        endpoint_halt(device_, endpoint);
        endpoint_flush(device_, endpoint);
        endpoint_clear(device_, endpoint);
        if (inflight_ > 0) xSemaphoreTake(idle_, portMAX_DELAY);
    }
    free_buffers();
}

void IsocOutStream::abort() {
    aborted_ = true;
    if (space_) xSemaphoreGive(space_);
}

void IsocOutStream::free_buffers() {
    for (Transfer*& transfer : transfers_) {
        if (transfer) transfer_free(transfer);
        transfer = nullptr;
    }
    heap_caps_free(ring_);
    ring_ = nullptr;
}

void IsocOutStream::release_one() {
    if (inflight_.fetch_sub(1) == 1) xSemaphoreGive(idle_);
}

void IsocOutStream::done(Transfer* transfer) {
    auto* self = static_cast<IsocOutStream*>(transfer->context);
    if (self->stopping_ || transfer->status == TransferStatus::NoDevice ||
        transfer->status == TransferStatus::Canceled) {
        self->release_one();
        return;
    }
    self->fill(transfer);
    if (transfer_submit(transfer) != ESP_OK) self->release_one();
}

void IsocOutStream::fill(Transfer* transfer) {
    size_t offset = 0;
    size_t read = read_.load(std::memory_order_relaxed);
    const size_t written = written_.load(std::memory_order_acquire);
    for (int i = 0; i < packets_; i++) {
        const size_t bytes = clock_.next() * frame_bytes_;
        uint8_t* out = transfer->data_buffer + offset;
        size_t take = std::min(bytes, written - read);
        take -= take % frame_bytes_;
        const size_t at = read % ring_bytes_;
        const size_t first = std::min(take, ring_bytes_ - at);
        memcpy(out, ring_ + at, first);
        memcpy(out + first, ring_, take - first);
        memset(out + take, 0, bytes - take);
        read += take;
        transfer->isoc_packet_desc[i].num_bytes = static_cast<int>(bytes);
        offset += bytes;
    }
    transfer->num_bytes = static_cast<int>(offset);
    read_.store(read, std::memory_order_release);
    xSemaphoreGive(space_);
}

esp_err_t IsocOutStream::write(const void* data, size_t len) {
    const auto* in = static_cast<const uint8_t*>(data);
    int stalls = 0;
    while (len > 0) {
        if (aborted_) return ESP_ERR_NOT_FOUND;
        if (!running_) return ESP_ERR_INVALID_STATE;
        const size_t written = written_.load(std::memory_order_relaxed);
        const size_t space = ring_bytes_ - (written - read_.load(std::memory_order_acquire));
        if (space == 0) {
            if (xSemaphoreTake(space_, pdMS_TO_TICKS(kWriteWaitMs)) != pdTRUE &&
                ++stalls >= kWriteStalls) {
                return ESP_ERR_TIMEOUT;
            }
            continue;
        }
        stalls = 0;
        const size_t take = std::min(space, len);
        const size_t at = written % ring_bytes_;
        const size_t first = std::min(take, ring_bytes_ - at);
        memcpy(ring_ + at, in, first);
        memcpy(ring_, in + first, take - first);
        written_.store(written + take, std::memory_order_release);
        in += take;
        len -= take;
    }
    return ESP_OK;
}

void IsocOutStream::drain() {
    int stalls = 0;
    while (running_ && !aborted_ &&
           written_.load(std::memory_order_acquire) - read_.load(std::memory_order_acquire) >=
               frame_bytes_) {
        if (xSemaphoreTake(space_, pdMS_TO_TICKS(kWriteWaitMs)) != pdTRUE &&
            ++stalls >= kWriteStalls) {
            return;
        }
    }
    if (running_ && !aborted_) vTaskDelay(pdMS_TO_TICKS(kTransfers * transfer_ms_));
}

}  // namespace usb_host::detail
