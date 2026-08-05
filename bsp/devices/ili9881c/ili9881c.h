/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ILI9881C MIPI-DSI panel driver. Supplies the panel timings and initialization
 * sequence to the generic mipi_dsi driver, which exposes the result as a
 * bsp_display provider (BSP_DISPLAY_TYPE_MIPI_DSI).
 *
 * Backlight is a PWM via LEDC on backlight_gpio (0..100% via set_brightness).
 * The driver does not touch panel reset — boards that wire RESET through an
 * I/O expander assert it during bring-up before calling ili9881c_lcd_create().
 */

#pragma once
#include "bsp_display.h"
#include "driver/gpio.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    bsp_size_t         size;            /*!< panel native geometry (px)         */
    bsp_pixel_format_t pixel_format;    /*!< RGB565 or RGB888                   */
    uint8_t            fb_num;          /*!< number of host framebuffers (1..3) */
    gpio_num_t         backlight_gpio;  /*!< LEDC PWM pin; NC -> no backlight   */
} ili9881c_config_t;

BSP_NONNULL(1, 2) esp_err_t ili9881c_lcd_create(const ili9881c_config_t *config, bsp_display_t **out);

#ifdef __cplusplus
}
#endif
