/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <cstddef>
#include <cstdint>

namespace usb_host::detail {

struct __attribute__((packed)) SetupPacket {
    uint8_t bmRequestType;
    uint8_t bRequest;
    uint16_t wValue;
    uint16_t wIndex;
    uint16_t wLength;
};
static_assert(sizeof(SetupPacket) == 8);

struct __attribute__((packed)) StandardDesc {
    uint8_t bLength;
    uint8_t bDescriptorType;
};

struct __attribute__((packed)) DeviceDesc {
    uint8_t bLength;
    uint8_t bDescriptorType;
    uint16_t bcdUSB;
    uint8_t bDeviceClass;
    uint8_t bDeviceSubClass;
    uint8_t bDeviceProtocol;
    uint8_t bMaxPacketSize0;
    uint16_t idVendor;
    uint16_t idProduct;
    uint16_t bcdDevice;
    uint8_t iManufacturer;
    uint8_t iProduct;
    uint8_t iSerialNumber;
    uint8_t bNumConfigurations;
};
static_assert(sizeof(DeviceDesc) == 18);

struct __attribute__((packed)) ConfigDesc {
    uint8_t bLength;
    uint8_t bDescriptorType;
    uint16_t wTotalLength;
    uint8_t bNumInterfaces;
    uint8_t bConfigurationValue;
    uint8_t iConfiguration;
    uint8_t bmAttributes;
    uint8_t bMaxPower;
};

struct __attribute__((packed)) InterfaceDesc {
    uint8_t bLength;
    uint8_t bDescriptorType;
    uint8_t bInterfaceNumber;
    uint8_t bAlternateSetting;
    uint8_t bNumEndpoints;
    uint8_t bInterfaceClass;
    uint8_t bInterfaceSubClass;
    uint8_t bInterfaceProtocol;
    uint8_t iInterface;
};

struct __attribute__((packed)) EndpointDesc {
    uint8_t bLength;
    uint8_t bDescriptorType;
    uint8_t bEndpointAddress;
    uint8_t bmAttributes;
    uint16_t wMaxPacketSize;
    uint8_t bInterval;
};

enum class TransferType : uint8_t { Control = 0, Isochronous = 1, Bulk = 2, Interrupt = 3 };
enum class Speed : uint8_t { Low, Full, High };

constexpr uint8_t kDescDevice = 0x01;
constexpr uint8_t kDescConfig = 0x02;
constexpr uint8_t kDescString = 0x03;
constexpr uint8_t kDescInterface = 0x04;
constexpr uint8_t kDescEndpoint = 0x05;

constexpr uint8_t kReqGetStatus = 0x00;
constexpr uint8_t kReqClearFeature = 0x01;
constexpr uint8_t kReqSetAddress = 0x05;
constexpr uint8_t kReqGetDescriptor = 0x06;
constexpr uint8_t kReqSetConfiguration = 0x09;
constexpr uint8_t kReqSetInterface = 0x0b;
constexpr uint16_t kFeatureEndpointHalt = 0x00;

constexpr uint8_t kReqDirIn = 0x80;
constexpr uint8_t kReqTypeStandard = 0x00;
constexpr uint8_t kReqTypeClass = 0x20;
constexpr uint8_t kReqRecipDevice = 0x00;
constexpr uint8_t kReqRecipInterface = 0x01;
constexpr uint8_t kReqRecipEndpoint = 0x02;

constexpr uint8_t kClassAudio = 0x01;
constexpr uint8_t kClassMassStorage = 0x08;
constexpr uint8_t kClassVideo = 0x0e;

constexpr uint8_t kEpDirIn = 0x80;
constexpr uint8_t kEpSyncMask = 0x0c;
constexpr uint8_t kEpSyncAsync = 0x04;
constexpr uint8_t kEpUsageMask = 0x30;
constexpr uint8_t kEpUsageFeedback = 0x10;

inline bool ep_is_in(const EndpointDesc* ep) { return ep->bEndpointAddress & kEpDirIn; }
inline TransferType ep_type(const EndpointDesc* ep) {
    return static_cast<TransferType>(ep->bmAttributes & 0x03);
}
inline uint16_t ep_mps(const EndpointDesc* ep) { return ep->wMaxPacketSize & 0x07ff; }
inline uint8_t ep_mult(const EndpointDesc* ep) { return (ep->wMaxPacketSize >> 11) & 0x03; }

// The descriptor after `desc` within the configuration, or null at its end.
const StandardDesc* next_descriptor(const ConfigDesc* config, const StandardDesc* desc);

}  // namespace usb_host::detail
