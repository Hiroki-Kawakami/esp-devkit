/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "tab5_power.h"
#include <stdlib.h>
#include "ina226/ina226.h"

#define INA226_ADDR       0x41
#define INA226_SHUNT_MOHM 5

#define CHG_QC_EN_N_PIN 5
#define CHG_EN_PIN      7

#define CHARGING_MAX_MA  (-50)
#define DISCHARGE_MIN_MA 10

#define BATT_EMPTY_MV   6600u
#define BATT_FULL_MV    8200u

typedef struct {
    bsp_power_t base;
    ina226_handle_t ina226;
    pi4io_t io;
} tab5_power_state_t;

static esp_err_t get_battery_voltage(bsp_power_t *self, uint32_t *out_mv) {
    tab5_power_state_t *s = (tab5_power_state_t *)self;
    return ina226_get_bus_voltage_mv(s->ina226, out_mv);
}

static esp_err_t get_battery_level(bsp_power_t *self, uint8_t *out_percent) {
    tab5_power_state_t *s = (tab5_power_state_t *)self;
    uint32_t mv;
    esp_err_t err = ina226_get_bus_voltage_mv(s->ina226, &mv);
    if (err != ESP_OK) return err;
    if (mv <= BATT_EMPTY_MV) *out_percent = 0;
    else if (mv >= BATT_FULL_MV) *out_percent = 100;
    else *out_percent = (uint8_t)((mv - BATT_EMPTY_MV) * 100 / (BATT_FULL_MV - BATT_EMPTY_MV));
    return ESP_OK;
}

static esp_err_t set_charge(bsp_power_t *self, bsp_power_charge_t mode) {
    tab5_power_state_t *s = (tab5_power_state_t *)self;
    esp_err_t err = pi4io_set_output(s->io, CHG_QC_EN_N_PIN, mode != BSP_POWER_CHARGE_FAST);
    if (err != ESP_OK) return err;
    return pi4io_set_output(s->io, CHG_EN_PIN, mode != BSP_POWER_CHARGE_OFF);
}

/* No signal reports the external supply, and CHG_STAT (PI4IOE2 pin 6, the
 * IP2326 LED pin) reads high in every state without a pull, so the status
 * comes from the INA226 battery current. Charge current reads negative. */
static bsp_power_status_t get_status(bsp_power_t *self) {
    tab5_power_state_t *s = (tab5_power_state_t *)self;
    int32_t ma = 0;
    if (ina226_get_current_ma(s->ina226, &ma) != ESP_OK) return BSP_POWER_STATUS_UNKNOWN;
    if (ma <= CHARGING_MAX_MA) return BSP_POWER_STATUS_CHARGING;
    if (ma < DISCHARGE_MIN_MA) return BSP_POWER_STATUS_EXTERNAL;
    return BSP_POWER_STATUS_BATTERY;
}

static esp_err_t deinit(bsp_power_t *self) {
    tab5_power_state_t *s = (tab5_power_state_t *)self;
    esp_err_t err = ina226_destroy(s->ina226);
    free(s);
    return err;
}

esp_err_t tab5_power_create(const tab5_power_config_t *config, bsp_power_t **out_power) {
    if (!config || !out_power || !config->i2c_bus || !config->io_expander) {
        return ESP_ERR_INVALID_ARG;
    }

    tab5_power_state_t *s = calloc(1, sizeof(*s));
    if (!s) return ESP_ERR_NO_MEM;

    esp_err_t err = ina226_create(&(ina226_config_t){
        .i2c_bus     = config->i2c_bus,
        .i2c_address = INA226_ADDR,
        .shunt_mohm  = INA226_SHUNT_MOHM,
    }, &s->ina226);
    if (err != ESP_OK) {
        free(s);
        return err;
    }
    s->io = config->io_expander;

    s->base.caps = BSP_POWER_CAP_BATTERY | BSP_POWER_CAP_EXTERNAL | BSP_POWER_CAP_CHARGE_CTRL
                 | BSP_POWER_CAP_CHARGE_FAST | BSP_POWER_CAP_CHARGE_STAT;
    s->base.get_battery_voltage = get_battery_voltage;
    s->base.get_battery_level   = get_battery_level;
    s->base.get_status          = get_status;
    s->base.set_charge          = set_charge;
    s->base.deinit              = deinit;

    *out_power = &s->base;
    return ESP_OK;
}
