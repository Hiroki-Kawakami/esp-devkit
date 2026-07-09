/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Internal LED-driver interface. Same struct-inheritance vtable as
 * bsp_display / bsp_touch / bsp_rtc: a driver embeds bsp_led_t as its first
 * member and returns &state->base from its *_create(). Boards register the
 * provider once via bsp_led_set_active() from bsp_init.
 */

#pragma once
#include "bsp_types.h"

typedef struct bsp_led bsp_led_t;

struct bsp_led {
    uint8_t   count;                  /*!< number of individually-addressable LEDs */
    /* A provider implements the one it backs natively; the other stays NULL and
     * surfaces as ESP_ERR_NOT_SUPPORTED. RGB backs set_brightness as a white scale. */
    esp_err_t (*set_rgb)(bsp_led_t *self, uint8_t index, uint8_t r, uint8_t g, uint8_t b);
    esp_err_t (*set_brightness)(bsp_led_t *self, uint8_t index, uint8_t level);
    esp_err_t (*clear)(bsp_led_t *self);
    esp_err_t (*deinit)(bsp_led_t *self);
};

/* Board's bsp_init() calls this once after creating its provider; NULL is
 * allowed (no LED -> bsp_led_count() returns 0). */
void bsp_led_set_active(bsp_led_t *led);
