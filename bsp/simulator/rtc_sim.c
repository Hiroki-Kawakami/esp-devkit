/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host-side bsp_rtc: get_time reads the host clock; set_time is a no-op and the
 * countdown timer ops are absent (NULL -> ESP_ERR_NOT_SUPPORTED).
 */

#include "rtc_sim.h"
#include <time.h>

static esp_err_t sim_get_time(bsp_rtc_t *self, bsp_rtc_datetime_t *out) {
    (void)self;
    time_t now = time(NULL);
    struct tm tm;
    localtime_r(&now, &tm);
    out->year    = (uint16_t)(tm.tm_year + 1900);
    out->month   = (uint8_t)(tm.tm_mon + 1);
    out->day     = (uint8_t)tm.tm_mday;
    out->hour    = (uint8_t)tm.tm_hour;
    out->minute  = (uint8_t)tm.tm_min;
    out->second  = (uint8_t)tm.tm_sec;
    out->weekday = (uint8_t)tm.tm_wday;
    return ESP_OK;
}

static esp_err_t sim_set_time(bsp_rtc_t *self, const bsp_rtc_datetime_t *dt) {
    (void)self; (void)dt;
    return ESP_OK;
}

static esp_err_t sim_time_is_valid(bsp_rtc_t *self, bool *out_valid) {
    (void)self;
    *out_valid = true;
    return ESP_OK;
}

static esp_err_t sim_deinit(bsp_rtc_t *self) { (void)self; return ESP_OK; }

static bsp_rtc_t s_sim_rtc = {
    .get_time      = sim_get_time,
    .set_time      = sim_set_time,
    .time_is_valid = sim_time_is_valid,
    .deinit        = sim_deinit,
};

esp_err_t rtc_sim_create(bsp_rtc_t **out_rtc) {
    *out_rtc = &s_sim_rtc;
    return ESP_OK;
}
