/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Internal RTC-driver interface. Same struct-inheritance vtable as bsp_display /
 * bsp_touch: a driver embeds bsp_rtc_t first and returns &state->base from its
 * *_create(). Timer/INT ops are optional (NULL -> ESP_ERR_NOT_SUPPORTED).
 */

#pragma once
#include "bsp_types.h"
#include "bsp.h"   /* bsp_rtc_int_cb_t */

typedef struct bsp_rtc bsp_rtc_t;

struct bsp_rtc {
    esp_err_t (*get_time)(bsp_rtc_t *self, bsp_rtc_datetime_t *out);
    esp_err_t (*set_time)(bsp_rtc_t *self, const bsp_rtc_datetime_t *dt);
    esp_err_t (*time_is_valid)(bsp_rtc_t *self, bool *out_valid);
    esp_err_t (*deinit)(bsp_rtc_t *self);

    /* Optional. */
    esp_err_t (*timer_start)(bsp_rtc_t *self, uint32_t seconds, bool repeat);
    esp_err_t (*timer_stop)(bsp_rtc_t *self);
    esp_err_t (*timer_is_expired)(bsp_rtc_t *self, bool *out_expired);
    esp_err_t (*timer_clear)(bsp_rtc_t *self);
    esp_err_t (*set_int_cb)(bsp_rtc_t *self, bsp_rtc_int_cb_t cb, void *arg);
};

/* Board's bsp_init() calls this once after creating its provider; NULL is allowed
 * (no RTC -> bsp_rtc_is_available() returns false). */
void bsp_rtc_set_active(bsp_rtc_t *rtc);
