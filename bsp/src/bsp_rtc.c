/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Model-agnostic RTC layer: holds the active bsp_rtc provider (registered by the
 * board's bsp_init via bsp_rtc_set_active) and implements the public bsp_rtc_*
 * API by dispatching through its vtable. The mirror of src/bsp_touch.c.
 */

#include "bsp.h"
#include "bsp_rtc.h"

static bsp_rtc_t *s_rtc;

void bsp_rtc_set_active(bsp_rtc_t *rtc) {
    s_rtc = rtc;
}

bool bsp_rtc_is_available(void) {
    return s_rtc != NULL;
}

esp_err_t bsp_rtc_get_time(bsp_rtc_datetime_t *out) {
    if (!s_rtc) return ESP_ERR_INVALID_STATE;
    return s_rtc->get_time(s_rtc, out);
}

esp_err_t bsp_rtc_set_time(const bsp_rtc_datetime_t *dt) {
    if (!s_rtc) return ESP_ERR_INVALID_STATE;
    return s_rtc->set_time(s_rtc, dt);
}

esp_err_t bsp_rtc_time_is_valid(bool *out_valid) {
    if (!s_rtc) return ESP_ERR_INVALID_STATE;
    return s_rtc->time_is_valid(s_rtc, out_valid);
}

esp_err_t bsp_rtc_timer_start(uint32_t ms, bool repeat) {
    if (!s_rtc) return ESP_ERR_INVALID_STATE;
    if (!s_rtc->timer_start) return ESP_ERR_NOT_SUPPORTED;
    return s_rtc->timer_start(s_rtc, ms, repeat);
}

esp_err_t bsp_rtc_timer_stop(void) {
    if (!s_rtc) return ESP_ERR_INVALID_STATE;
    if (!s_rtc->timer_stop) return ESP_ERR_NOT_SUPPORTED;
    return s_rtc->timer_stop(s_rtc);
}

esp_err_t bsp_rtc_timer_is_expired(bool *out_expired) {
    if (!s_rtc) return ESP_ERR_INVALID_STATE;
    if (!s_rtc->timer_is_expired) return ESP_ERR_NOT_SUPPORTED;
    return s_rtc->timer_is_expired(s_rtc, out_expired);
}

esp_err_t bsp_rtc_timer_clear(void) {
    if (!s_rtc) return ESP_ERR_INVALID_STATE;
    if (!s_rtc->timer_clear) return ESP_ERR_NOT_SUPPORTED;
    return s_rtc->timer_clear(s_rtc);
}

esp_err_t bsp_rtc_set_int_cb(bsp_rtc_int_cb_t cb, void *arg) {
    if (!s_rtc) return ESP_ERR_INVALID_STATE;
    if (!s_rtc->set_int_cb) return ESP_ERR_NOT_SUPPORTED;
    return s_rtc->set_int_cb(s_rtc, cb, arg);
}
