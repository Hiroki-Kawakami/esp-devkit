/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <cstdint>
#include <vector>

#include "esp_err.h"
#include "usb/usb_types_ch9.h"
#include "usb_host_uac.hpp"

namespace usb_host::detail {

struct UacStreamAlt {
    uint8_t interface = 0;
    uint8_t alternate = 0;
    uint8_t endpoint = 0;
    uint16_t max_packet_bytes = 0;
    uint8_t interval = 0;
    bool rate_control = false;
    UacFormat format;
};

// Channel bit n is logical channel n; bit 0 is the master control.
struct UacFeatureUnit {
    uint8_t id = 0;
    uint8_t mute_channels = 0;
    uint8_t volume_channels = 0;
};

struct UacTopology {
    uint8_t control_interface = 0;
    std::vector<UacStreamAlt> alts;
    UacFeatureUnit feature;
};

// The first UAC1 PCM streaming interface in the given direction. Playback
// takes adaptive and synchronous endpoints only and fills in the feature unit;
// capture leaves it empty. ESP_ERR_NOT_SUPPORTED when there is none.
esp_err_t uac_parse(const usb_config_desc_t* config, bool capture, UacTopology* out);

}  // namespace usb_host::detail
