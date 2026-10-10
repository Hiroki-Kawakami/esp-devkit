/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "usb_device_vendor.hpp"

namespace usb_device {

namespace {

constexpr uint8_t kClassVendor = 0xff;
constexpr uint16_t kBulkHighSpeedBytes = 512;
constexpr uint16_t kBulkFullSpeedBytes = 64;
constexpr uint8_t kRecipientMask = 0x1f;
constexpr uint8_t kRecipientInterface = 0x01;

}  // namespace

void Vendor::describe(ConfigBuilder& config) {
    const uint16_t packet =
        config.speed() == Speed::High ? kBulkHighSpeedBytes : kBulkFullSpeedBytes;
    interface_ = config.interface(kClassVendor, 0, 0, config_.name);
    if (config_.winusb) config.winusb(config_.device_interface_guid);
    if (config_.out) out_address_ = config.endpoint(EndpointType::Bulk, false, packet);
    if (config_.in) in_address_ = config.endpoint(EndpointType::Bulk, true, packet);
}

void Vendor::configured() {
    if (config_.configured) config_.configured();
}

void Vendor::unconfigured() {
    if (config_.unconfigured) config_.unconfigured();
}

bool Vendor::control(const ControlRequest& request, uint8_t* data, size_t* bytes) {
    if ((request.request_type & kRecipientMask) == kRecipientInterface &&
        (request.index & 0xff) != interface_) {
        return false;
    }
    return config_.control && config_.control(request, data, bytes);
}

}  // namespace usb_device
