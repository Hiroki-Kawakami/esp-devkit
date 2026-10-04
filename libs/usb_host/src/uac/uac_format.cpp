/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <algorithm>

#include "usb_host_uac.hpp"

namespace usb_host {

bool UacFormat::supports(uint32_t rate) const {
    if (rates.empty()) return rate >= min_rate && rate <= max_rate;
    return std::find(rates.begin(), rates.end(), rate) != rates.end();
}

}  // namespace usb_host
