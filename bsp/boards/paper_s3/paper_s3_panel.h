/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5PaperS3 Panel (Display + Touch). The touch I2C bus is brought up by
 * paper_s3.c and passed in (NULL -> touch skipped).
 */

#pragma once
#include "bsp.h"
#include "bsp_display.h"
#include "bsp_touch.h"
#include "driver/i2c_master.h"

esp_err_t paper_s3_panel_init(const bsp_config_t *config, i2c_master_bus_handle_t i2c_bus);
