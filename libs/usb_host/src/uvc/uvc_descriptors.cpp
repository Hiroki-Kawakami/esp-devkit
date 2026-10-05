/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "uvc_descriptors.hpp"

#include "esp_log.h"
#include "usb/usb_helpers.h"

namespace usb_host::detail {

namespace {

const char* TAG = "usb_host_uvc";

constexpr uint8_t kClassVideo = 0x0e;
constexpr uint8_t kSubclassControl = 0x01;
constexpr uint8_t kSubclassStreaming = 0x02;
constexpr uint8_t kCsInterface = 0x24;

constexpr uint8_t kVcHeader = 0x01;
constexpr uint8_t kVsFormatMjpeg = 0x06;
constexpr uint8_t kVsFrameMjpeg = 0x07;

constexpr int kFrameFixedBytes = 26;

uint16_t le16(const uint8_t* bytes) {
    return static_cast<uint16_t>(bytes[0] | (bytes[1] << 8));
}

uint32_t le32(const uint8_t* bytes) {
    return bytes[0] | (bytes[1] << 8) | (bytes[2] << 16) | (static_cast<uint32_t>(bytes[3]) << 24);
}

void parse_frame(const uint8_t* bytes, uint8_t length, UvcTopology* out) {
    if (length < kFrameFixedBytes) return;
    UvcFrameDesc frame;
    frame.index = bytes[3];
    frame.size.width = le16(bytes + 5);
    frame.size.height = le16(bytes + 7);
    frame.max_frame_bytes = le32(bytes + 17);
    const int count = bytes[25];
    if (count == 0) {
        if (length < kFrameFixedBytes + 12) return;
        frame.size.min_interval = le32(bytes + 26);
        frame.size.max_interval = le32(bytes + 30);
        frame.size.step_interval = le32(bytes + 34);
    } else {
        for (int i = 0; i < count && kFrameFixedBytes + i * 4 + 4 <= length; i++) {
            frame.size.intervals.push_back(le32(bytes + kFrameFixedBytes + i * 4));
        }
        if (frame.size.intervals.empty()) return;
    }
    out->frames.push_back(std::move(frame));
}

void parse_endpoint(const usb_ep_desc_t* endpoint, uint8_t alternate, UvcTopology* out) {
    if (!USB_EP_DESC_GET_EP_DIR(endpoint)) return;
    const int type = USB_EP_DESC_GET_XFERTYPE(endpoint);
    if (type == USB_TRANSFER_TYPE_BULK && alternate == 0) {
        out->bulk_endpoint = endpoint->bEndpointAddress;
        out->bulk_max_packet_bytes = USB_EP_DESC_GET_MPS(endpoint);
        return;
    }
    if (type != USB_TRANSFER_TYPE_ISOCHRONOUS || alternate == 0) return;
    if (USB_EP_DESC_GET_MULT(endpoint) != 0) {
        ESP_LOGW(TAG, "alt %u: %u transactions per microframe not supported", alternate,
                 USB_EP_DESC_GET_MULT(endpoint) + 1);
        return;
    }
    UvcIsocAlt alt;
    alt.alternate = alternate;
    alt.endpoint = endpoint->bEndpointAddress;
    alt.max_packet_bytes = USB_EP_DESC_GET_MPS(endpoint);
    alt.interval = endpoint->bInterval;
    if (alt.max_packet_bytes && alt.interval) out->isoc_alts.push_back(alt);
}

}  // namespace

esp_err_t uvc_parse(const usb_config_desc_t* config, UvcTopology* out) {
    *out = {};
    bool in_control = false;
    bool in_streaming = false;
    bool in_mjpeg = false;
    bool done = false;
    int streaming = -1;
    uint8_t alternate = 0;

    int offset = 0;
    auto* desc = reinterpret_cast<const usb_standard_desc_t*>(config);
    while ((desc = usb_parse_next_descriptor(desc, config->wTotalLength, &offset))) {
        const auto* bytes = reinterpret_cast<const uint8_t*>(desc);
        const uint8_t length = desc->bLength;
        if (desc->bDescriptorType == USB_B_DESCRIPTOR_TYPE_INTERFACE) {
            const auto* interface = reinterpret_cast<const usb_intf_desc_t*>(desc);
            const bool video = interface->bInterfaceClass == kClassVideo;
            in_control = video && interface->bInterfaceSubClass == kSubclassControl;
            in_mjpeg = false;
            if (streaming >= 0 && interface->bInterfaceNumber != streaming) {
                done = done || !out->frames.empty();
                if (!done) {
                    out->isoc_alts.clear();
                    out->bulk_endpoint = 0;
                    streaming = -1;
                }
            }
            in_streaming = !done && video && interface->bInterfaceSubClass == kSubclassStreaming;
            if (in_streaming) {
                streaming = interface->bInterfaceNumber;
                alternate = interface->bAlternateSetting;
            }
        } else if (desc->bDescriptorType == kCsInterface && in_control && length >= 5 &&
                   bytes[2] == kVcHeader) {
            out->uvc_version = le16(bytes + 3);
        } else if (desc->bDescriptorType == kCsInterface && in_streaming && alternate == 0 &&
                   length >= 4) {
            if (bytes[2] == kVsFormatMjpeg) {
                in_mjpeg = out->frames.empty();
                if (in_mjpeg) out->format_index = bytes[3];
            } else if (bytes[2] == kVsFrameMjpeg && in_mjpeg) {
                parse_frame(bytes, length, out);
            } else if (bytes[2] != kVsFrameMjpeg) {
                in_mjpeg = false;
            }
        } else if (desc->bDescriptorType == USB_B_DESCRIPTOR_TYPE_ENDPOINT && in_streaming) {
            parse_endpoint(reinterpret_cast<const usb_ep_desc_t*>(desc), alternate, out);
        }
    }

    if (out->frames.empty() || (out->isoc_alts.empty() && !out->bulk_endpoint)) {
        *out = {};
        return ESP_ERR_NOT_SUPPORTED;
    }
    out->streaming_interface = static_cast<uint8_t>(streaming);
    return ESP_OK;
}

}  // namespace usb_host::detail
