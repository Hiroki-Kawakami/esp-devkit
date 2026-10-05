/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <cstdint>

#include "sdkconfig.h"
#include "usb_host.hpp"
#ifdef ESP_PLATFORM
#include "usb_core.hpp"
#endif

namespace usb_host::detail {

const Callbacks& callbacks();

#if CONFIG_USBH_MSC
esp_err_t msc_install();
#ifdef ESP_PLATFORM
void msc_connected(uint8_t address);
void msc_gone(Device* device);
#endif
#endif

#if CONFIG_USBH_UAC
esp_err_t uac_install();
#ifdef ESP_PLATFORM
void uac_connected(uint8_t address);
void uac_gone(Device* device);
#endif
#endif

#if CONFIG_USBH_UVC
esp_err_t uvc_install();
#ifdef ESP_PLATFORM
void uvc_connected(uint8_t address);
void uvc_gone(Device* device);
#endif
#endif

}  // namespace usb_host::detail
