/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "transfer.hpp"

#include "esp_cache.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_memory_utils.h"
#include "esp_private/esp_cache_private.h"

namespace usb_host::detail {

namespace {

const char* TAG = "usb_host";

size_t cache_alignment(uint32_t caps) {
    size_t alignment = 0;
    esp_cache_get_alignment(caps, &alignment);
    return alignment ? alignment : 4;
}

esp_err_t status_to_err(usb_transfer_status_t status) {
    switch (status) {
        case USB_TRANSFER_STATUS_COMPLETED:
            return ESP_OK;
        case USB_TRANSFER_STATUS_STALL:
            return ESP_ERR_INVALID_RESPONSE;
        case USB_TRANSFER_STATUS_NO_DEVICE:
            return ESP_ERR_NOT_FOUND;
        case USB_TRANSFER_STATUS_TIMED_OUT:
            return ESP_ERR_TIMEOUT;
        default:
            return ESP_FAIL;
    }
}

}  // namespace

TransferContext::~TransferContext() {
    if (done_) vSemaphoreDelete(done_);
}

esp_err_t TransferContext::init(usb_host_client_handle_t client) {
    client_ = client;
    dma_alignment_ = cache_alignment(MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL);
    psram_alignment_ = cache_alignment(MALLOC_CAP_DMA | MALLOC_CAP_SPIRAM);
    done_ = xSemaphoreCreateBinary();
    return done_ ? ESP_OK : ESP_ERR_NO_MEM;
}

void TransferContext::done(usb_transfer_t* transfer) {
    auto* context = static_cast<TransferContext*>(transfer->context);
    xSemaphoreGive(context->done_);
}

esp_err_t TransferContext::await(usb_transfer_t* transfer, uint32_t timeout_ms) {
    if (xSemaphoreTake(done_, pdMS_TO_TICKS(timeout_ms)) != pdTRUE) {
        usb_host_endpoint_halt(device_, transfer->bEndpointAddress);
        usb_host_endpoint_flush(device_, transfer->bEndpointAddress);
        usb_host_endpoint_clear(device_, transfer->bEndpointAddress);
        xSemaphoreTake(done_, portMAX_DELAY);
        return ESP_ERR_TIMEOUT;
    }
    return status_to_err(transfer->status);
}

esp_err_t TransferContext::submit(usb_transfer_t* transfer, uint8_t endpoint, size_t bytes,
                                  uint32_t timeout_ms) {
    transfer->device_handle = device_;
    transfer->bEndpointAddress = endpoint;
    transfer->callback = done;
    transfer->context = this;
    transfer->timeout_ms = timeout_ms;
    transfer->num_bytes = static_cast<int>(bytes);
    const esp_err_t err = usb_host_transfer_submit(transfer);
    if (err != ESP_OK) return err;
    return await(transfer, timeout_ms);
}

esp_err_t TransferContext::control(usb_transfer_t* transfer, uint8_t request_type,
                                   uint8_t request, uint16_t value, uint16_t index,
                                   uint16_t length, uint32_t timeout_ms) {
    auto* setup = reinterpret_cast<usb_setup_packet_t*>(transfer->data_buffer);
    setup->bmRequestType = request_type;
    setup->bRequest = request;
    setup->wValue = value;
    setup->wIndex = index;
    setup->wLength = length;
    transfer->device_handle = device_;
    transfer->bEndpointAddress = 0;
    transfer->callback = done;
    transfer->context = this;
    transfer->timeout_ms = timeout_ms;
    transfer->num_bytes = static_cast<int>(sizeof(usb_setup_packet_t) + length);
    const esp_err_t err = usb_host_transfer_submit_control(client_, transfer);
    if (err != ESP_OK) return err;
    return await(transfer, timeout_ms);
}

esp_err_t TransferContext::clear_halt(usb_transfer_t* control_transfer, uint8_t endpoint,
                                      uint32_t timeout_ms) {
    esp_err_t err = usb_host_endpoint_halt(device_, endpoint);
    if (err != ESP_OK) return err;
    err = usb_host_endpoint_flush(device_, endpoint);
    if (err != ESP_OK) return err;
    err = usb_host_endpoint_clear(device_, endpoint);
    if (err != ESP_OK) return err;
    return control(control_transfer,
                   USB_BM_REQUEST_TYPE_DIR_OUT | USB_BM_REQUEST_TYPE_TYPE_STANDARD |
                       USB_BM_REQUEST_TYPE_RECIP_ENDPOINT,
                   USB_B_REQUEST_CLEAR_FEATURE, ENDPOINT_HALT, endpoint, 0, timeout_ms);
}

/* The host stack allocates transfer buffers itself, but hcd_dwc's
 * cache_sync_data_buffer() documents that class drivers may overwrite
 * data_buffer; swapping it in place is how a transfer lands straight in a
 * caller's buffer instead of being copied out of the stack's own. An IN
 * transfer is synced with ESP_CACHE_MSYNC_FLAG_DIR_M2C (no UNALIGNED), so a
 * borrowed buffer must be cache aligned in both address and size. */
void TransferContext::set_buffer(usb_transfer_t* transfer, void* buffer, size_t bytes) {
    *const_cast<uint8_t**>(&transfer->data_buffer) = static_cast<uint8_t*>(buffer);
    *const_cast<size_t*>(&transfer->data_buffer_size) = bytes;
}

/* A PSRAM destination has to be kept coherent by hand (see the sync
   functions), and that is only possible when the buffer starts and ends on a
   cache line, so the requirement is stricter there than for internal RAM. */
bool TransferContext::borrowable(const void* buffer, size_t bytes, uint16_t mps) const {
    if (!buffer || !bytes) return false;
    const size_t alignment = esp_ptr_external_ram(buffer) ? psram_alignment_ : dma_alignment_;
    return reinterpret_cast<uintptr_t>(buffer) % alignment == 0 && bytes % alignment == 0 &&
           bytes % mps == 0;
}

/* The DMA reaches PSRAM behind the cache, so a borrowed buffer's lines have to
   go out before the transfer -- otherwise a later write-back lands on top of
   what arrived -- and be dropped after a read, or the CPU keeps seeing what it
   cached before. Internal RAM needs neither. Getting this wrong does not fail:
   it returns a buffer that is right except for the lines the cache happened to
   hold, which reads as a file that is subtly different every time it is
   read. */
void TransferContext::sync_for_device(void* buffer, size_t bytes) {
    if (!esp_ptr_external_ram(buffer)) return;
    const esp_err_t err = esp_cache_msync(buffer, bytes, ESP_CACHE_MSYNC_FLAG_DIR_C2M);
    if (err != ESP_OK) ESP_LOGE(TAG, "cache writeback: %s", esp_err_to_name(err));
}

void TransferContext::sync_for_cpu(void* buffer, size_t bytes) {
    if (!esp_ptr_external_ram(buffer)) return;
    const esp_err_t err = esp_cache_msync(buffer, bytes, ESP_CACHE_MSYNC_FLAG_DIR_M2C);
    if (err != ESP_OK) ESP_LOGE(TAG, "cache invalidate: %s", esp_err_to_name(err));
}

}  // namespace usb_host::detail
