/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "transfer.hpp"

#include "esp_heap_caps.h"
#include "esp_memory_utils.h"
#include "esp_private/esp_cache_private.h"

namespace usb_host::detail {

namespace {

size_t cache_alignment(uint32_t caps) {
    size_t alignment = 0;
    esp_cache_get_alignment(caps, &alignment);
    return alignment ? alignment : 4;
}

esp_err_t status_to_err(TransferStatus status) {
    switch (status) {
        case TransferStatus::Completed:
            return ESP_OK;
        case TransferStatus::Stall:
            return ESP_ERR_INVALID_RESPONSE;
        case TransferStatus::NoDevice:
            return ESP_ERR_NOT_FOUND;
        case TransferStatus::TimedOut:
            return ESP_ERR_TIMEOUT;
        default:
            return ESP_FAIL;
    }
}

}  // namespace

TransferContext::~TransferContext() {
    if (done_) vSemaphoreDelete(done_);
}

esp_err_t TransferContext::init() {
    dma_alignment_ = cache_alignment(MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL);
    psram_alignment_ = cache_alignment(MALLOC_CAP_DMA | MALLOC_CAP_SPIRAM);
    done_ = xSemaphoreCreateBinary();
    return done_ ? ESP_OK : ESP_ERR_NO_MEM;
}

void TransferContext::done(Transfer* transfer) {
    auto* context = static_cast<TransferContext*>(transfer->context);
    xSemaphoreGive(context->done_);
}

esp_err_t TransferContext::await(Transfer* transfer, uint32_t timeout_ms) {
    if (xSemaphoreTake(done_, pdMS_TO_TICKS(timeout_ms)) != pdTRUE) {
        endpoint_halt(device_, transfer->bEndpointAddress);
        endpoint_flush(device_, transfer->bEndpointAddress);
        endpoint_clear(device_, transfer->bEndpointAddress);
        xSemaphoreTake(done_, portMAX_DELAY);
        return ESP_ERR_TIMEOUT;
    }
    return status_to_err(transfer->status);
}

esp_err_t TransferContext::submit(Transfer* transfer, uint8_t endpoint, size_t bytes,
                                  uint32_t timeout_ms) {
    transfer->device = device_;
    transfer->bEndpointAddress = endpoint;
    transfer->callback = done;
    transfer->context = this;
    transfer->num_bytes = static_cast<int>(bytes);
    xSemaphoreTake(done_, 0);
    const esp_err_t err = transfer_submit(transfer);
    if (err != ESP_OK) return err;
    return await(transfer, timeout_ms);
}

esp_err_t TransferContext::control(Transfer* transfer, uint8_t request_type, uint8_t request,
                                   uint16_t value, uint16_t index, uint16_t length,
                                   uint32_t timeout_ms) {
    auto* setup = reinterpret_cast<SetupPacket*>(transfer->data_buffer);
    setup->bmRequestType = request_type;
    setup->bRequest = request;
    setup->wValue = value;
    setup->wIndex = index;
    setup->wLength = length;
    transfer->device = device_;
    transfer->bEndpointAddress = 0;
    transfer->callback = done;
    transfer->context = this;
    transfer->num_bytes = static_cast<int>(sizeof(SetupPacket) + length);
    xSemaphoreTake(done_, 0);
    const esp_err_t err = transfer_submit_control(transfer);
    if (err != ESP_OK) return err;
    return await(transfer, timeout_ms);
}

esp_err_t TransferContext::clear_halt(Transfer* control_transfer, uint8_t endpoint,
                                      uint32_t timeout_ms) {
    esp_err_t err = endpoint_halt(device_, endpoint);
    if (err == ESP_OK) err = endpoint_flush(device_, endpoint);
    if (err == ESP_OK) err = endpoint_clear(device_, endpoint);
    if (err != ESP_OK) return err;
    return control(control_transfer, kReqTypeStandard | kReqRecipEndpoint, kReqClearFeature,
                   kFeatureEndpointHalt, endpoint, 0, timeout_ms);
}

/* The host stack syncs the cache around every transfer, and an IN buffer is
   invalidated whole, so a borrowed buffer must start and end on a cache line. */
bool TransferContext::borrowable(const void* buffer, size_t bytes, uint16_t mps) const {
    if (!buffer || !bytes) return false;
    const size_t alignment = esp_ptr_external_ram(buffer) ? psram_alignment_ : dma_alignment_;
    return reinterpret_cast<uintptr_t>(buffer) % alignment == 0 && bytes % alignment == 0 &&
           bytes % mps == 0;
}

}  // namespace usb_host::detail
