/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "in_stream.hpp"

#include <algorithm>

#include "esp_heap_caps.h"
#include "esp_log.h"

namespace usb_host::detail {

namespace {

const char* TAG = "usb_host";

constexpr uint32_t kStopTimeoutMs = 500;

}  // namespace

InStream::~InStream() {
    stop();
    if (idle_) vSemaphoreDelete(idle_);
}

esp_err_t InStream::init() {
    idle_ = xSemaphoreCreateBinary();
    return idle_ ? ESP_OK : ESP_ERR_NO_MEM;
}

esp_err_t InStream::start(Device* device, const InStreamConfig& config, Sink sink,
                          void* context) {
    stop();
    const int count = std::min(config.transfers, kMaxTransfers);
    if (count < 1 || !config.max_packet_bytes) return ESP_ERR_INVALID_ARG;
    if (config.isochronous ? config.packets < 1
                           : config.transfer_bytes % config.max_packet_bytes != 0) {
        return ESP_ERR_INVALID_ARG;
    }
    device_ = device;
    config_ = config;
    sink_ = sink;
    context_ = context;

    const size_t bytes = config.isochronous
                             ? static_cast<size_t>(config.packets) * config.max_packet_bytes
                             : config.transfer_bytes;
    const int packets = config.isochronous ? config.packets : 0;
    // Internal RAM: while other masters keep PSRAM busy, the controller cannot
    // drain its RX FIFO into it in time, and then misses the (micro)frames of
    // every isochronous endpoint on the bus.
    const uint32_t caps = MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL;
    for (int i = 0; i < count; i++) {
        Transfer*& transfer = transfers_[i];
        const esp_err_t err = transfer_alloc(bytes, packets, caps, &transfer);
        if (err != ESP_OK) {
            free_transfers();
            return err;
        }
        transfer->device = device_;
        transfer->bEndpointAddress = config.endpoint;
        transfer->callback = done;
        transfer->context = this;
        transfer->num_bytes = static_cast<int>(bytes);
        for (int p = 0; p < packets; p++) {
            transfer->isoc_packet_desc[p].num_bytes = config.max_packet_bytes;
        }
    }

    xSemaphoreTake(idle_, 0);
    running_ = true;
    for (int i = 0; i < count; i++) {
        inflight_++;
        const esp_err_t err = transfer_submit(transfers_[i]);
        if (err != ESP_OK) {
            ESP_LOGE(TAG, "IN submit: %s", esp_err_to_name(err));
            inflight_--;
            stop();
            return err;
        }
    }
    return ESP_OK;
}

void InStream::stop() {
    if (!transfers_[0]) return;
    running_ = false;
    if (inflight_ > 0) {
        endpoint_halt(device_, config_.endpoint);
        endpoint_flush(device_, config_.endpoint);
        endpoint_clear(device_, config_.endpoint);
        if (inflight_ > 0 && xSemaphoreTake(idle_, pdMS_TO_TICKS(kStopTimeoutMs)) != pdTRUE) {
            ESP_LOGE(TAG, "IN transfers did not return");
            xSemaphoreTake(idle_, portMAX_DELAY);
        }
    }
    free_transfers();
}

void InStream::free_transfers() {
    for (Transfer*& transfer : transfers_) {
        transfer_free(transfer);
        transfer = nullptr;
    }
}

void InStream::release_one() {
    if (inflight_.fetch_sub(1) == 1) xSemaphoreGive(idle_);
}

void InStream::done(Transfer* transfer) {
    auto* self = static_cast<InStream*>(transfer->context);
    if (!self->running_ || transfer->status == TransferStatus::NoDevice ||
        transfer->status == TransferStatus::Canceled) {
        if (self->running_) {
            ESP_LOGW(TAG, "IN %02x: transfer ended with status %d, not resubmitted",
                     transfer->bEndpointAddress, static_cast<int>(transfer->status));
        }
        self->release_one();
        return;
    }
    self->deliver(transfer);
    const bool failed = !self->config_.isochronous &&
                        transfer->status != TransferStatus::Completed;
    if (failed) {
        ESP_LOGW(TAG, "bulk IN %02x: status %d", transfer->bEndpointAddress,
                 static_cast<int>(transfer->status));
    }
    if (failed || !self->running_) {
        self->release_one();
        return;
    }
    const esp_err_t err = transfer_submit(transfer);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "IN %02x: resubmit: %s", transfer->bEndpointAddress, esp_err_to_name(err));
        self->release_one();
    }
}

void InStream::deliver(Transfer* transfer) {
    if (!config_.isochronous) {
        const bool ok = transfer->status == TransferStatus::Completed;
        sink_(context_, transfer->data_buffer, ok ? transfer->actual_num_bytes : 0,
              transfer->num_bytes, ok);
        return;
    }
    const uint8_t* packet = transfer->data_buffer;
    for (int i = 0; i < transfer->num_isoc_packets; i++) {
        const IsocPacket& desc = transfer->isoc_packet_desc[i];
        switch (desc.status) {
            case TransferStatus::Completed:
                if (desc.actual_num_bytes > 0) {
                    sink_(context_, packet, desc.actual_num_bytes, desc.num_bytes, true);
                }
                break;
            case TransferStatus::TimedOut:
            case TransferStatus::Skipped:
                break;
            default:
                sink_(context_, nullptr, 0, desc.num_bytes, false);
                break;
        }
        packet += desc.num_bytes;
    }
}

}  // namespace usb_host::detail
