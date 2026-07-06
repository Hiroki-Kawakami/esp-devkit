/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Model-agnostic power layer: dispatches the bsp_power_* sensing API through the
 * active provider (registered via bsp_power_set_active), plus weak defaults for
 * the power controls that a board with real power hardware overrides.
 */

#include "bsp.h"
#include "bsp_power.h"

static bsp_power_t *s_power;

void bsp_power_set_active(bsp_power_t *power) {
    s_power = power;
}

uint32_t bsp_power_get_caps(void) {
    return s_power ? s_power->caps : 0;
}

esp_err_t bsp_power_get_battery_voltage(uint32_t *out_mv) {
    if (!s_power || !s_power->get_battery_voltage) return ESP_ERR_NOT_SUPPORTED;
    return s_power->get_battery_voltage(s_power, out_mv);
}

esp_err_t bsp_power_get_battery_level(uint8_t *out_percent) {
    if (!s_power || !s_power->get_battery_level) return ESP_ERR_NOT_SUPPORTED;
    return s_power->get_battery_level(s_power, out_percent);
}

bool bsp_power_vbus_present(void) {
    if (!s_power || !s_power->vbus_present) return false;
    return s_power->vbus_present(s_power);
}

__attribute__((weak)) esp_err_t bsp_power_off(void) {
    return ESP_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) esp_err_t bsp_power_set_switch(bsp_power_switch_t sw, bool on) {
    (void)sw;
    (void)on;
    return ESP_ERR_NOT_SUPPORTED;
}

#ifdef ESP_PLATFORM
#include "esp_system.h"

__attribute__((weak)) void bsp_power_restart(void) {
    esp_restart();
}

__attribute__((weak)) esp_err_t bsp_power_hw_reset(void) {
    esp_restart();
    return ESP_FAIL;
}
#else
#include <stdlib.h>

__attribute__((weak)) void bsp_power_restart(void) {
    exit(0);
}

__attribute__((weak)) esp_err_t bsp_power_hw_reset(void) {
    exit(0);
}
#endif
