/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "in_stream.hpp"

#include <algorithm>

#include "esp_cache.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_private/esp_cache_private.h"
#include "esp_timer.h"
#include "transfer.hpp"

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

esp_err_t InStream::start(usb_device_handle_t device, const InStreamConfig& config, Sink sink,
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
    size_t alignment = 0;
    esp_cache_get_alignment(MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL, &alignment);
    alignment = std::max<size_t>(alignment, 4);
    const size_t internal_bytes = (bytes + alignment - 1) / alignment * alignment;
    for (int i = 0; i < count; i++) {
        usb_transfer_t*& transfer = transfers_[i];
        const esp_err_t err = usb_host_transfer_alloc(bytes, packets, &transfer);
        if (err != ESP_OK) {
            free_transfers();
            return err;
        }
        /* Isochronous data goes to internal RAM: with the host stack's buffers in
           PSRAM, the controller's writes lose to video decode and scanout for
           PSRAM bandwidth, and packets are skipped or never scheduled. */
        if (config.isochronous) {
            auto* internal = static_cast<uint8_t*>(heap_caps_aligned_alloc(
                alignment, internal_bytes, MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL));
            if (!internal) {
                free_transfers();
                return ESP_ERR_NO_MEM;
            }
            own_buffers_[i] = transfer->data_buffer;
            own_bytes_[i] = transfer->data_buffer_size;
            TransferContext::set_buffer(transfer, internal, internal_bytes);
        }
        transfer->device_handle = device_;
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
        const esp_err_t err = usb_host_transfer_submit(transfers_[i]);
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
        usb_host_endpoint_halt(device_, config_.endpoint);
        usb_host_endpoint_flush(device_, config_.endpoint);
        usb_host_endpoint_clear(device_, config_.endpoint);
        if (inflight_ > 0 && xSemaphoreTake(idle_, pdMS_TO_TICKS(kStopTimeoutMs)) != pdTRUE) {
            ESP_LOGE(TAG, "IN transfers did not return");
            xSemaphoreTake(idle_, portMAX_DELAY);
        }
    }
    free_transfers();
}

void InStream::free_transfers() {
    for (int i = 0; i < kMaxTransfers; i++) {
        usb_transfer_t*& transfer = transfers_[i];
        if (transfer && own_buffers_[i]) {
            heap_caps_free(transfer->data_buffer);
            TransferContext::set_buffer(transfer, own_buffers_[i], own_bytes_[i]);
            own_buffers_[i] = nullptr;
        }
        if (transfer) usb_host_transfer_free(transfer);
        transfer = nullptr;
    }
}

void InStream::release_one() {
    if (inflight_.fetch_sub(1) == 1) xSemaphoreGive(idle_);
}

InStream::Stats InStream::take_stats() {
    Stats stats;
    stats.transfers = transfers_done_.exchange(0);
    stats.packets_ok = packets_ok_.exchange(0);
    stats.packets_skipped = packets_skipped_.exchange(0);
    stats.packets_failed = packets_failed_.exchange(0);
    stats.packets_error = packets_error_.exchange(0);
    stats.packets_overflow = packets_overflow_.exchange(0);
    stats.packets_stall = packets_stall_.exchange(0);
    stats.failed_bytes_min = failed_bytes_min_.exchange(-1);
    stats.failed_bytes_max = failed_bytes_max_.exchange(-1);
    stats.not_resubmitted = not_resubmitted_.exchange(0);
    stats.inflight = inflight_.load();
    stats.last_done_us = last_done_us_.load();
    stats.last_status = last_status_.load();
    return stats;
}

void InStream::done(usb_transfer_t* transfer) {
    auto* self = static_cast<InStream*>(transfer->context);
    self->transfers_done_++;
    self->last_done_us_ = esp_timer_get_time();
    self->last_status_ = static_cast<uint8_t>(transfer->status);
    if (!self->running_ || transfer->status == USB_TRANSFER_STATUS_NO_DEVICE ||
        transfer->status == USB_TRANSFER_STATUS_CANCELED) {
        if (self->running_) {
            self->not_resubmitted_++;
            ESP_LOGW(TAG, "IN %02x: transfer ended with status %d, not resubmitted",
                     transfer->bEndpointAddress, transfer->status);
        }
        self->release_one();
        return;
    }
    self->deliver(transfer);
    const bool failed = !self->config_.isochronous &&
                        transfer->status != USB_TRANSFER_STATUS_COMPLETED;
    if (failed) ESP_LOGW(TAG, "bulk IN %02x: status %d", transfer->bEndpointAddress, transfer->status);
    if (failed || !self->running_) {
        if (failed) self->not_resubmitted_++;
        self->release_one();
        return;
    }
    const esp_err_t err = usb_host_transfer_submit(transfer);
    if (err != ESP_OK) {
        self->not_resubmitted_++;
        ESP_LOGW(TAG, "IN %02x: resubmit: %s", transfer->bEndpointAddress, esp_err_to_name(err));
        self->release_one();
    }
}

void InStream::deliver(usb_transfer_t* transfer) {
    if (!config_.isochronous) {
        const bool ok = transfer->status == USB_TRANSFER_STATUS_COMPLETED;
        sink_(context_, transfer->data_buffer, ok ? transfer->actual_num_bytes : 0,
              transfer->num_bytes, ok);
        return;
    }
    const uint8_t* packet = transfer->data_buffer;
    for (int i = 0; i < transfer->num_isoc_packets; i++) {
        const usb_isoc_packet_desc_t& desc = transfer->isoc_packet_desc[i];
        switch (desc.status) {
            case USB_TRANSFER_STATUS_COMPLETED:
                packets_ok_++;
                if (desc.actual_num_bytes > 0) {
                    sink_(context_, packet, desc.actual_num_bytes, desc.num_bytes, true);
                }
                break;
            case USB_TRANSFER_STATUS_TIMED_OUT:
            case USB_TRANSFER_STATUS_SKIPPED:
                packets_skipped_++;
                break;
            default: {
                packets_failed_++;
                if (desc.status == USB_TRANSFER_STATUS_ERROR) packets_error_++;
                if (desc.status == USB_TRANSFER_STATUS_OVERFLOW) packets_overflow_++;
                if (desc.status == USB_TRANSFER_STATUS_STALL) packets_stall_++;
                const int bytes = desc.actual_num_bytes;
                const int min = failed_bytes_min_.load();
                const int max = failed_bytes_max_.load();
                if (min < 0 || bytes < min) failed_bytes_min_ = bytes;
                if (bytes > max) failed_bytes_max_ = bytes;
                sink_(context_, nullptr, 0, desc.num_bytes, false);
                break;
            }
        }
        packet += desc.num_bytes;
    }
}

}  // namespace usb_host::detail
