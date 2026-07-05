/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host-side bsp_power: level from SIMULATOR_BATTERY_PERCENT (default 76),
 * voltage derived linearly over a 1S Li-ion range, VBUS from SIMULATOR_VBUS
 * (present unless "0") -- the host behaves like a USB-powered device.
 */

#include "power_sim.h"
#include <stdlib.h>
#include <string.h>

#define SIM_BATT_EMPTY_MV 3300u
#define SIM_BATT_FULL_MV  4200u

static uint8_t sim_level(void) {
    const char *env = getenv("SIMULATOR_BATTERY_PERCENT");
    long v = env ? strtol(env, NULL, 10) : 76;
    if (v < 0) v = 0;
    if (v > 100) v = 100;
    return (uint8_t)v;
}

static esp_err_t sim_get_battery_level(bsp_power_t *self, uint8_t *out_percent) {
    (void)self;
    *out_percent = sim_level();
    return ESP_OK;
}

static esp_err_t sim_get_battery_voltage(bsp_power_t *self, uint32_t *out_mv) {
    (void)self;
    *out_mv = SIM_BATT_EMPTY_MV + (uint32_t)sim_level() * (SIM_BATT_FULL_MV - SIM_BATT_EMPTY_MV) / 100u;
    return ESP_OK;
}

static bool sim_vbus_present(bsp_power_t *self) {
    (void)self;
    const char *env = getenv("SIMULATOR_VBUS");
    return !(env && strcmp(env, "0") == 0);
}

static bsp_power_t s_power = {
    .caps                = BSP_POWER_CAP_BATTERY | BSP_POWER_CAP_VBUS,
    .get_battery_voltage = sim_get_battery_voltage,
    .get_battery_level   = sim_get_battery_level,
    .vbus_present        = sim_vbus_present,
    .deinit              = NULL,
};

esp_err_t power_sim_create(bsp_power_t **out_power) {
    *out_power = &s_power;
    return ESP_OK;
}
