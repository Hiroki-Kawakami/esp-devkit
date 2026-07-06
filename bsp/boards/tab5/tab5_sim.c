/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Tab5 simulator board: the host-side counterpart of tab5.c. Brings up
 * the shared SDL backend in MIPI mode with the Tab5's native portrait geometry.
 */

#include "bsp.h"
#include "bsp_display.h"
#include "bsp_touch.h"
#include "sdl_panel.h"
#include <stdlib.h>

#define TAB5_PANEL_W 720
#define TAB5_PANEL_H 1280

esp_err_t bsp_init(const bsp_config_t *config) {
    sdl_panel_config_t sdl_config = {
        .title     = "M5Stack Tab5",
        .type      = BSP_DISPLAY_TYPE_MIPI_DSI,
        .size      = { TAB5_PANEL_W, TAB5_PANEL_H },
        .format    = config->display.pixel_format == BSP_PIXEL_FORMAT_RGB888
                     ? BSP_PIXEL_FORMAT_RGB888 : BSP_PIXEL_FORMAT_RGB565,
        .scale_div = 2,
        .framebuffer = { .fb_num = config->display.fb_num ? config->display.fb_num : 2 },
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

esp_err_t bsp_power_off(void) { exit(0); }
