/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * AtomS3 Lite simulator board. The device has no display and the LED/button
 * are physical-only, so there is nothing to render: bsp_init is a no-op and the
 * LED/button providers stay unregistered (bsp_led_count/bsp_button_count == 0).
 */

#include "bsp.h"

esp_err_t bsp_init(const bsp_config_t *config) {
    (void)config;
    return ESP_OK;
}
