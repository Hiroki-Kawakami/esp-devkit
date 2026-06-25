/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Paper (ESP32) Panel (Display + Touch). The shared SPI bus is already up
 * (paper.c) by the time this runs.
 */

#pragma once
#include "bsp.h"
#include "bsp_display.h"
#include "bsp_touch.h"

esp_err_t paper_panel_init(const bsp_config_t *config);
