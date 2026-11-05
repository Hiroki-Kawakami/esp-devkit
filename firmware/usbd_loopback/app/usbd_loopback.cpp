/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "usbd_loopback.hpp"

#include <algorithm>
#include <cstring>
#include <memory>

#include "esp_heap_caps.h"
#include "esp_log.h"
#include "usb_device.hpp"
#include "usb_device_vendor.hpp"

namespace {

const char* TAG = "usbd_loopback";

constexpr uint16_t kVendorId = 0x1209;
constexpr uint16_t kProductId = 0x0001;
// One packet past the longest message, so a message of exactly that length
// still ends at its zero-length packet instead of leaving it for the next.
constexpr size_t kBufferBytes = 16384 + 512;
constexpr size_t kBufferAlign = 128;
constexpr size_t kStoreBytes = 256;
constexpr uint8_t kRequestTypeMask = 0x60;
constexpr uint8_t kRequestTypeVendor = 0x40;
constexpr uint8_t kDirIn = 0x80;
constexpr uint8_t kReqPattern = 0x01;
constexpr uint8_t kReqStore = 0x02;
constexpr uint8_t kReqLoad = 0x03;

std::shared_ptr<usb_device::Vendor> s_vendor;
std::unique_ptr<usb_device::Device> s_device;
uint8_t* s_buffer;
usb_device::Transfer s_out;
usb_device::Transfer s_in;
uint8_t s_store[kStoreBytes];
size_t s_stored;

void out_done(usb_device::Transfer* transfer);
void in_done(usb_device::Transfer* transfer);

void receive() {
    s_out = {};
    s_out.buffer = s_buffer;
    s_out.length = kBufferBytes;
    s_out.callback = out_done;
    if (s_vendor->out().submit(&s_out) != ESP_OK) ESP_LOGE(TAG, "OUT submit failed");
}

void out_done(usb_device::Transfer* transfer) {
    if (transfer->status != usb_device::TransferStatus::Completed) return;
    s_in = {};
    s_in.buffer = s_buffer;
    s_in.length = transfer->actual;
    s_in.zlp = true;
    s_in.callback = in_done;
    if (s_vendor->in().submit(&s_in) != ESP_OK) ESP_LOGE(TAG, "IN submit failed");
}

void in_done(usb_device::Transfer* transfer) {
    if (transfer->status == usb_device::TransferStatus::Completed) receive();
}

bool control(const usb_device::ControlRequest& request, uint8_t* data, size_t* bytes) {
    if ((request.request_type & kRequestTypeMask) != kRequestTypeVendor) return false;
    const bool in = request.request_type & kDirIn;
    switch (request.request) {
        case kReqPattern: {
            if (!in) return false;
            const size_t n = std::min<size_t>(request.length, *bytes);
            for (size_t i = 0; i < n; i++) data[i] = static_cast<uint8_t>(i);
            *bytes = n;
            return true;
        }
        case kReqStore:
            if (in) return false;
            s_stored = std::min<size_t>(request.length, kStoreBytes);
            memcpy(s_store, data, s_stored);
            return true;
        case kReqLoad: {
            if (!in) return false;
            const size_t n = std::min({static_cast<size_t>(request.length), *bytes, s_stored});
            memcpy(data, s_store, n);
            *bytes = n;
            return true;
        }
        default:
            return false;
    }
}

}  // namespace

void usbd_loopback_start() {
    s_buffer = static_cast<uint8_t*>(
        heap_caps_aligned_alloc(kBufferAlign, kBufferBytes, MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL));
    if (!s_buffer) {
        ESP_LOGE(TAG, "no buffer");
        return;
    }

    usb_device::Vendor::Config vendor;
    vendor.name = "loopback";
    vendor.configured = [] {
        ESP_LOGI(TAG, "configured");
        receive();
    };
    vendor.unconfigured = [] { ESP_LOGI(TAG, "unconfigured"); };
    vendor.control = control;
    s_vendor = std::make_shared<usb_device::Vendor>(std::move(vendor));

    usb_device::DeviceConfig config;
    config.info.vendor_id = kVendorId;
    config.info.product_id = kProductId;
    config.info.bcd_device = 0x0100;
    config.info.manufacturer = "esp-devkit";
    config.info.product = "usbd_loopback";
    config.info.serial = "loopback-1";
    s_device = std::make_unique<usb_device::Device>(std::move(config));
    esp_err_t err = s_device->add(s_vendor);
    if (err == ESP_OK) err = s_device->start();
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "device: %s", esp_err_to_name(err));
        return;
    }
    ESP_LOGI(TAG, "started");
}
