/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Model-agnostic LED layer: holds the active bsp_led provider (registered by
 * the board's bsp_init via bsp_led_set_active) and implements the public
 * bsp_led_* API by dispatching through its vtable.
 */

#include "bsp.h"
#include "bsp_led.h"

static bsp_led_t *s_led;

void bsp_led_set_active(bsp_led_t *led) {
    s_led = led;
}

uint8_t bsp_led_count(void) {
    return s_led ? s_led->count : 0;
}

esp_err_t bsp_led_set_rgb(uint8_t index, uint8_t r, uint8_t g, uint8_t b) {
    if (!s_led) return ESP_ERR_INVALID_STATE;
    if (index >= s_led->count) return ESP_ERR_INVALID_ARG;
    if (!s_led->set_rgb) return ESP_ERR_NOT_SUPPORTED;
    return s_led->set_rgb(s_led, index, r, g, b);
}

esp_err_t bsp_led_set_brightness(uint8_t index, uint8_t level) {
    if (!s_led) return ESP_ERR_INVALID_STATE;
    if (index >= s_led->count) return ESP_ERR_INVALID_ARG;
    if (!s_led->set_brightness) return ESP_ERR_NOT_SUPPORTED;
    return s_led->set_brightness(s_led, index, level);
}

esp_err_t bsp_led_clear(void) {
    if (!s_led) return ESP_ERR_INVALID_STATE;
    return s_led->clear(s_led);
}
