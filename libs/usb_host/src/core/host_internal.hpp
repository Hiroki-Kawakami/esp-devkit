/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <cstdint>

#include "sdkconfig.h"
#include "usb_host.hpp"
#ifdef ESP_PLATFORM
#include "usb/usb_host.h"
#endif

namespace usb_host::detail {

const Callbacks& callbacks();

#ifdef ESP_PLATFORM
usb_host_client_handle_t client();
// A client may open a device only once, so classes sharing a composite
// device share one handle; the last close_device() closes it.
esp_err_t open_device(uint8_t address, usb_device_handle_t* out);
void close_device(usb_device_handle_t handle);
#endif

#if CONFIG_USBH_MSC
esp_err_t msc_install();
#ifdef ESP_PLATFORM
void msc_connected(uint8_t address);
void msc_gone(usb_device_handle_t handle);
#endif
#endif

#if CONFIG_USBH_UAC
esp_err_t uac_install();
#ifdef ESP_PLATFORM
void uac_connected(uint8_t address);
void uac_gone(usb_device_handle_t handle);
#endif
#endif

#if CONFIG_USBH_UVC
esp_err_t uvc_install();
#ifdef ESP_PLATFORM
void uvc_connected(uint8_t address);
void uvc_gone(usb_device_handle_t handle);
#endif
#endif

}  // namespace usb_host::detail
