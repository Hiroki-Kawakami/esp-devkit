/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Core display bring-up: split from core.c so the entry orchestrates
 * buses while this owns the ILI9342C wiring (SPI, GPIO reset, LEDC backlight).
 */

#pragma once
#include "bsp.h"

#ifdef __cplusplus
extern "C" {
#endif

esp_err_t core_panel_init(void);

#ifdef __cplusplus
}
#endif
