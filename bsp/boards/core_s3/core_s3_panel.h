/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack CoreS3 panel bring-up: split from core_s3.c so the device entry
 * orchestrates buses / power / IO expander while this owns the LCD (and, later,
 * touch) wiring. The backlight LDO lives on the AXP2101 and the LCD reset on the
 * AW9523B, so both handles are passed in.
 */

#pragma once
#include "bsp.h"
#include "driver/i2c_master.h"
#include "axp2101.h"
#include "aw9523.h"

#ifdef __cplusplus
extern "C" {
#endif

esp_err_t core_s3_panel_init(i2c_master_bus_handle_t i2c_bus,
                             axp2101_handle_t axp, aw9523_t aw);

#ifdef __cplusplus
}
#endif
