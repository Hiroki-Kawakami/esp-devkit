/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Generic RGB-parallel LCD provider (esp_lcd_new_rgb_panel). RGB panels have no
 * command interface -- pixels flow continuously from a PSRAM framebuffer over
 * the parallel bus, so there is no init sequence and no chip-specific driver.
 * The panel is fully described by its geometry, timings, and pin map; this
 * driver takes those as config and returns a bsp_display provider (type
 * BSP_DISPLAY_TYPE_RGB) that the board registers with bsp_display_set_active().
 *
 * Backlight is a PWM via LEDC on backlight_gpio (0..100% via set_brightness);
 * NC skips backlight control. RESET / DISP_EN are handled by the board (through
 * its own GPIO/expander wiring) before rgb_lcd_create() runs -- this driver only
 * owns the RGB peripheral itself.
 */

#pragma once
#include "bsp_display.h"
#include "driver/gpio.h"

#ifdef __cplusplus
extern "C" {
#endif

#define RGB_LCD_MAX_DATA_LINES 16   /* enough for RGB565; RGB888 would need 24 */

typedef struct {
    uint16_t hsync_pulse_width;
    uint16_t hsync_back_porch;
    uint16_t hsync_front_porch;
    uint16_t vsync_pulse_width;
    uint16_t vsync_back_porch;
    uint16_t vsync_front_porch;
    bool     pclk_active_neg;       /*!< latch pixel on the falling PCLK edge */
} rgb_lcd_timing_t;

typedef struct {
    bsp_size_t         size;                        /*!< panel resolution (px)          */
    bsp_pixel_format_t pixel_format;                /*!< RGB565 only for now            */
    uint32_t           pclk_hz;                     /*!< pixel clock (Hz)                */
    uint8_t            data_width;                  /*!< active data lines (e.g. 16)     */
    uint8_t            fb_num;                      /*!< host framebuffers (1..3)        */
    bool               fb_in_psram;                 /*!< true when the FB won't fit in SRAM */
    uint16_t           dma_burst_size;              /*!< 0 -> IDF default                */
    rgb_lcd_timing_t   timing;
    gpio_num_t         data_gpios[RGB_LCD_MAX_DATA_LINES]; /*!< D0..Dn (unused = NC)     */
    gpio_num_t         hsync_gpio;
    gpio_num_t         vsync_gpio;
    gpio_num_t         de_gpio;                     /*!< NC if HV-sync only              */
    gpio_num_t         pclk_gpio;
    gpio_num_t         disp_gpio;                   /*!< panel-level display enable; NC if unused */
    gpio_num_t         backlight_gpio;              /*!< LEDC PWM pin; NC to skip        */
} rgb_lcd_config_t;

BSP_NONNULL(1, 2) esp_err_t rgb_lcd_create(const rgb_lcd_config_t *config, bsp_display_t **out);

#ifdef __cplusplus
}
#endif
