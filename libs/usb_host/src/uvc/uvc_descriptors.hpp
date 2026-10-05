/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <cstdint>
#include <vector>

#include "esp_err.h"
#include "usb/usb_types_ch9.h"
#include "usb_host_uvc.hpp"

namespace usb_host::detail {

struct UvcFrameDesc {
    uint8_t index = 0;
    uint32_t max_frame_bytes = 0;
    UvcFrameSize size;
};

struct UvcIsocAlt {
    uint8_t alternate = 0;
    uint8_t endpoint = 0;
    uint16_t max_packet_bytes = 0;
    uint8_t interval = 0;
};

struct UvcTopology {
    uint16_t uvc_version = 0;
    uint8_t streaming_interface = 0;
    uint8_t format_index = 0;
    std::vector<UvcFrameDesc> frames;
    // Single-transaction alternates only: the host stack issues one per
    // (micro)frame.
    std::vector<UvcIsocAlt> isoc_alts;
    uint8_t bulk_endpoint = 0;
    uint16_t bulk_max_packet_bytes = 0;
};

// ESP_ERR_NOT_SUPPORTED when no streaming interface offers MJPEG on an
// endpoint the host can use.
esp_err_t uvc_parse(const usb_config_desc_t* config, UvcTopology* out);

}  // namespace usb_host::detail
