/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * INA226 I2C current / bus-voltage monitor.
 */

#pragma once
#include "bsp_types.h"
#include "driver/i2c_master.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct ina226_dev *ina226_handle_t;

#define INA226_I2C_DEFAULT_HZ 400000

typedef struct {
    i2c_master_bus_handle_t i2c_bus;
    uint8_t i2c_address;   /*!< 7-bit, set by the A0/A1 straps */
    int     clock_hz;      /*!< 0 -> INA226_I2C_DEFAULT_HZ */
    uint32_t shunt_mohm;   /*!< shunt resistor; 0 -> current reads ESP_ERR_INVALID_STATE */
} ina226_config_t;

esp_err_t ina226_create(const ina226_config_t *cfg, ina226_handle_t *out_handle);
esp_err_t ina226_destroy(ina226_handle_t h);
esp_err_t ina226_get_bus_voltage_mv(ina226_handle_t h, uint32_t *out_mv);
/* Signed: positive when current flows IN+ -> IN-. */
esp_err_t ina226_get_current_ma(ina226_handle_t h, int32_t *out_ma);

#ifdef __cplusplus
}
#endif
