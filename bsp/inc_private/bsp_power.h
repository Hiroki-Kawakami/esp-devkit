/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Internal power-sensing interface. Same struct-inheritance vtable as
 * bsp_led / bsp_rtc: a driver embeds bsp_power_t as its first member and
 * returns &state->base from its *_create(). Boards register the provider once
 * via bsp_power_set_active() from bsp_init. Only the sensing seam; the power
 * controls (off / restart / hw_reset) are board free functions, not vtable ops.
 */

#pragma once
#include "bsp_types.h"

typedef struct bsp_power bsp_power_t;

struct bsp_power {
    uint32_t  caps;   /*!< BSP_POWER_CAP_* the provider actually implements */
    esp_err_t (*get_battery_voltage)(bsp_power_t *self, uint32_t *out_mv);
    esp_err_t (*get_battery_level)(bsp_power_t *self, uint8_t *out_percent);
    bool      (*vbus_present)(bsp_power_t *self);
    esp_err_t (*deinit)(bsp_power_t *self);
};

/* Board's bsp_init() calls this once after creating its provider; NULL is
 * allowed (no provider -> bsp_power_get_caps() returns 0). */
void bsp_power_set_active(bsp_power_t *power);
