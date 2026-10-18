/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "power_sim.h"
#include "bsp_harness.h"
#include <stdatomic.h>

#define DEFAULT_LEVEL    76
#define DEFAULT_EXTERNAL true

/* level in bits 0-7, external in bit 8: one word so a reader never sees a torn pair. */
static atomic_uint s_state = DEFAULT_LEVEL | (DEFAULT_EXTERNAL ? 1u << 8 : 0);
static atomic_bool s_charge_on = true;
static bool s_created;

typedef struct {
    bsp_power_t base;
    uint32_t empty_mv;
    uint32_t full_mv;
} power_sim_t;

static power_sim_t s_power;

static uint8_t sim_level(void) { return (uint8_t)(atomic_load(&s_state) & 0xFF); }

static esp_err_t sim_get_battery_level(bsp_power_t *self, uint8_t *out_percent) {
    (void)self;
    *out_percent = sim_level();
    return ESP_OK;
}

static esp_err_t sim_get_battery_voltage(bsp_power_t *self, uint32_t *out_mv) {
    power_sim_t *p = (power_sim_t *)self;
    *out_mv = p->empty_mv + (uint32_t)sim_level() * (p->full_mv - p->empty_mv) / 100u;
    return ESP_OK;
}

static bsp_power_status_t sim_get_status(bsp_power_t *self) {
    uint32_t state = atomic_load(&s_state);
    if (!((state >> 8) & 1)) return BSP_POWER_STATUS_BATTERY;
    if ((self->caps & BSP_POWER_CAP_CHARGE_STAT) && atomic_load(&s_charge_on) && (state & 0xFF) < 100) {
        return BSP_POWER_STATUS_CHARGING;
    }
    return BSP_POWER_STATUS_EXTERNAL;
}

static esp_err_t sim_set_charge(bsp_power_t *self, bsp_power_charge_t mode) {
    (void)self;
    atomic_store(&s_charge_on, mode != BSP_POWER_CHARGE_OFF);
    return ESP_OK;
}

esp_err_t power_sim_create(const power_sim_config_t *config, bsp_power_t **out_power) {
    if (!config || !out_power || config->full_mv <= config->empty_mv) return ESP_ERR_INVALID_ARG;
    s_power = (power_sim_t){
        .base = {
            .caps                = config->caps,
            .get_battery_voltage = sim_get_battery_voltage,
            .get_battery_level   = sim_get_battery_level,
        },
        .empty_mv = config->empty_mv,
        .full_mv  = config->full_mv,
    };
    if (config->caps & BSP_POWER_CAP_EXTERNAL)    s_power.base.get_status = sim_get_status;
    if (config->caps & BSP_POWER_CAP_CHARGE_CTRL) s_power.base.set_charge = sim_set_charge;
    s_created = true;
    *out_power = &s_power.base;
    return ESP_OK;
}

esp_err_t bsp_harness_sim_power_inject(const bsp_harness_sim_power_t *state) {
    if (!s_created) return ESP_ERR_NOT_SUPPORTED;
    if (state && state->level > 100) return ESP_ERR_INVALID_ARG;
    uint8_t level  = state ? state->level : DEFAULT_LEVEL;
    bool external  = state ? state->external : DEFAULT_EXTERNAL;
    atomic_store(&s_state, level | (external ? 1u << 8 : 0));
    return ESP_OK;
}
