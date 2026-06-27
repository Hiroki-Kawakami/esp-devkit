/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Paper (ESP32) Panel (Display + Touch). The shared SPI bus and the touch I2C
 * bus are already up (paper.c) by the time this runs; the I2C bus handle is
 * passed in for the touch controller (NULL -> touch skipped).
 */

#pragma once
#include "bsp.h"
#include "bsp_display.h"
#include "bsp_touch.h"
#include "driver/i2c_master.h"

esp_err_t paper_panel_init(const bsp_config_t *config, i2c_master_bus_handle_t i2c_bus);
