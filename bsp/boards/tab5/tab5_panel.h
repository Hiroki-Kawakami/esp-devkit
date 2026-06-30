/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Tab5 panel bring-up helpers: split from tab5.c so the device entry
 * orchestrates buses + IO expanders while this owns the LCD + touch wiring.
 */

#pragma once
#include "bsp.h"
#include "driver/i2c_master.h"

#ifdef __cplusplus
extern "C" {
#endif

esp_err_t tab5_panel_init(const bsp_config_t *config, i2c_master_bus_handle_t i2c_bus);

#ifdef __cplusplus
}
#endif
