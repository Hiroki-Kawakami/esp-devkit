/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Tab5 power provider: INA226 battery voltage / current plus the IP2326 charger
 * lines on PI4IOE2 (CHG_EN, nCHG_QC_EN).
 */

#pragma once
#include "bsp_power.h"
#include "driver/i2c_master.h"
#include "pi4io/pi4io.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    i2c_master_bus_handle_t i2c_bus;
    pi4io_t io_expander;        /*!< PI4IOE2 (nCHG_QC_EN pin 5, CHG_EN pin 7) */
} tab5_power_config_t;

esp_err_t tab5_power_create(const tab5_power_config_t *config, bsp_power_t **out_power);

#ifdef __cplusplus
}
#endif
