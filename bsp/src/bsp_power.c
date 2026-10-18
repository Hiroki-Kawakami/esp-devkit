/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Model-agnostic power layer: dispatches the bsp_power_* sensing / charger API through the
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

bsp_power_status_t bsp_power_get_status(void) {
    if (!s_power || !s_power->get_status) return BSP_POWER_STATUS_UNKNOWN;
    return s_power->get_status(s_power);
}

esp_err_t bsp_power_set_charge(bsp_power_charge_t mode) {
    if (!s_power || !s_power->set_charge) return ESP_ERR_NOT_SUPPORTED;
    if (mode == BSP_POWER_CHARGE_FAST && !(s_power->caps & BSP_POWER_CAP_CHARGE_FAST)) {
        return ESP_ERR_NOT_SUPPORTED;
    }
    return s_power->set_charge(s_power, mode);
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
