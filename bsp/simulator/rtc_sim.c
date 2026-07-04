/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host-side bsp_rtc: get_time reads the host clock offset by the last
 * set_time; `SIMULATOR_RTC_INVALID` (unset/"0" excluded) starts it invalid.
 */

#include "rtc_sim.h"
#include <stdlib.h>
#include <string.h>
#include <time.h>

static bool s_init_done = false;
static bool s_valid = false;
static time_t s_offset = 0;

static void ensure_init(void) {
    if (s_init_done) return;
    s_init_done = true;
    const char *env = getenv("SIMULATOR_RTC_INVALID");
    s_valid = !(env && strcmp(env, "0") != 0);
}

static esp_err_t sim_get_time(bsp_rtc_t *self, bsp_rtc_datetime_t *out) {
    (void)self;
    ensure_init();
    time_t now = time(NULL) + s_offset;
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
    (void)self;
    ensure_init();
    struct tm tm = {0};
    tm.tm_year  = dt->year - 1900;
    tm.tm_mon   = dt->month - 1;
    tm.tm_mday  = dt->day;
    tm.tm_hour  = dt->hour;
    tm.tm_min   = dt->minute;
    tm.tm_sec   = dt->second;
    tm.tm_isdst = -1;
    s_offset = mktime(&tm) - time(NULL);
    s_valid = true;
    return ESP_OK;
}

static esp_err_t sim_time_is_valid(bsp_rtc_t *self, bool *out_valid) {
    (void)self;
    ensure_init();
    *out_valid = s_valid;
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
