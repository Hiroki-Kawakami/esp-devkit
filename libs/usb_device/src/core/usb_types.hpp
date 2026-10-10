/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <cstdint>

namespace usb_device::detail {

struct __attribute__((packed)) SetupPacket {
    uint8_t bmRequestType;
    uint8_t bRequest;
    uint16_t wValue;
    uint16_t wIndex;
    uint16_t wLength;
};
static_assert(sizeof(SetupPacket) == 8);

constexpr uint8_t kDescDevice = 0x01;
constexpr uint8_t kDescConfig = 0x02;
constexpr uint8_t kDescString = 0x03;
constexpr uint8_t kDescInterface = 0x04;
constexpr uint8_t kDescEndpoint = 0x05;
constexpr uint8_t kDescDeviceQualifier = 0x06;
constexpr uint8_t kDescOtherSpeedConfig = 0x07;
constexpr uint8_t kDescInterfaceAssociation = 0x0b;
constexpr uint8_t kDescBos = 0x0f;
constexpr uint8_t kDescDeviceCapability = 0x10;

constexpr uint8_t kReqGetStatus = 0x00;
constexpr uint8_t kReqClearFeature = 0x01;
constexpr uint8_t kReqSetFeature = 0x03;
constexpr uint8_t kReqSetAddress = 0x05;
constexpr uint8_t kReqGetDescriptor = 0x06;
constexpr uint8_t kReqGetConfiguration = 0x08;
constexpr uint8_t kReqSetConfiguration = 0x09;
constexpr uint8_t kReqGetInterface = 0x0a;
constexpr uint8_t kReqSetInterface = 0x0b;

constexpr uint8_t kReqDirIn = 0x80;
constexpr uint8_t kReqTypeMask = 0x60;
constexpr uint8_t kReqTypeStandard = 0x00;
constexpr uint8_t kReqTypeVendor = 0x40;
constexpr uint8_t kReqRecipMask = 0x1f;
constexpr uint8_t kReqRecipDevice = 0x00;
constexpr uint8_t kReqRecipInterface = 0x01;
constexpr uint8_t kReqRecipEndpoint = 0x02;

constexpr uint8_t kEp0MaxPacket = 64;

}  // namespace usb_device::detail
