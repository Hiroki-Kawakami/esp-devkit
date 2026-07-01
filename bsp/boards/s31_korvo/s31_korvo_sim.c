/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ESP32-S31-Korvo simulator board: shared SDL backend in RGB mode with the
 * Korvo's native 800x480 landscape geometry.
 */

#include "bsp.h"
#include "bsp_display.h"
#include "bsp_touch.h"
#include "sdl_panel.h"
#include <stdlib.h>

#define S31_LCD_W 800
#define S31_LCD_H 480

esp_err_t bsp_init(const bsp_config_t *config) {
    sdl_panel_config_t sdl_config = {
        .title       = "ESP32-S31-Korvo",
        .type        = BSP_DISPLAY_TYPE_RGB,
        .size        = { S31_LCD_W, S31_LCD_H },
        .format      = BSP_PIXEL_FORMAT_RGB565,
        .scale_div   = 1,
        .framebuffer = { .fb_num = 2 },
    };
    bsp_display_t *display = NULL;
    bsp_touch_t   *touch   = NULL;
    esp_err_t err = sdl_panel_create(&sdl_config, &display, &touch);
    if (err != ESP_OK) return err;
    bsp_display_set_active(display);
    bsp_touch_set_active(touch);
    bsp_touch_start_reader(config->touch.task_priority, config->touch.task_affinity,
                           config->touch.poll_interval_ms, 0);
    return ESP_OK;
}

void bsp_restart(void)        { exit(0); }
esp_err_t bsp_hw_reset(void)  { exit(0); }
esp_err_t bsp_power_off(void) { exit(0); }
