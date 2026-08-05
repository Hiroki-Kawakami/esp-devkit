/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ST7121 MIPI-DSI panel driver for the 720x1280 Tab5 panel generation.
 * Supplies the panel timings and initialization sequence to the generic
 * mipi_dsi driver. Panel reset is handled by the board before create().
 */

#pragma once
#include "bsp_display.h"
#include "driver/gpio.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    bsp_size_t         size;
    bsp_pixel_format_t pixel_format;
    uint8_t            fb_num;
    gpio_num_t         backlight_gpio;
} st7121_config_t;

BSP_NONNULL(1, 2) esp_err_t st7121_lcd_create(const st7121_config_t *config, bsp_display_t **out);

#ifdef __cplusplus
}
#endif
