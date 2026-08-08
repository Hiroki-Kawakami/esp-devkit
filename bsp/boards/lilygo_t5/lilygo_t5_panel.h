/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "bsp.h"
#include "driver/i2c_master.h"

esp_err_t lilygo_t5_panel_init(const bsp_config_t *config,
                               i2c_master_bus_handle_t i2c_bus);
