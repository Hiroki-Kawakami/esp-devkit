/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <utility>

#include "host_internal.hpp"

namespace usb_host {

namespace {

bool s_installed;
Callbacks s_callbacks;

}  // namespace

const Callbacks& detail::callbacks() {
    return s_callbacks;
}

esp_err_t install(Callbacks callbacks) {
    if (s_installed) return ESP_ERR_INVALID_STATE;
    s_installed = true;
    s_callbacks = std::move(callbacks);
#if CONFIG_USBH_MSC
    if (const esp_err_t err = detail::msc_install(); err != ESP_OK) return err;
#endif
#if CONFIG_USBH_UAC
    if (const esp_err_t err = detail::uac_install(); err != ESP_OK) return err;
#endif
#if CONFIG_USBH_UVC
    if (const esp_err_t err = detail::uvc_install(); err != ESP_OK) return err;
#endif
    return ESP_OK;
}

}  // namespace usb_host
