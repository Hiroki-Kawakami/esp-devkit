/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ST7123 MIPI-DSI panel driver. Same shape as
 * the ILI9881C driver — allocates DSI bus + DBI io + DPI panel, owns the host
 * framebuffers, and exposes the result as a bsp_display provider. The driver
 * does not touch panel reset; boards drive it via the I/O expander before
 * calling st7123_lcd_create().
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
} st7123_config_t;

BSP_NONNULL(1, 2) esp_err_t st7123_lcd_create(const st7123_config_t *config, bsp_display_t **out);

#ifdef __cplusplus
}
#endif
