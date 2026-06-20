/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5PaperS3 simulator board: the host-side counterpart of paper_s3.c.
 */

#include "bsp.h"
#include "bsp_display.h"
#include "bsp_touch.h"
#include "sdl_panel.h"

esp_err_t bsp_init(const bsp_config_t *config) {
    sdl_panel_config_t sdl_config = {
        .title     = "M5PaperS3",
        .type      = BSP_DISPLAY_TYPE_DIRECT_EPD,
        .size      = { 960, 540 },
        .format    = BSP_PIXEL_FORMAT_L8,
        .scale_div = 2,
    };

    bsp_display_t *display = NULL;
    bsp_touch_t   *touch   = NULL;
    esp_err_t err = sdl_panel_create(&sdl_config, &display, &touch);
    if (err != ESP_OK) return err;

    bsp_display_set_active(display);
    bsp_touch_set_active(touch);
    return ESP_OK;
}

void bsp_restart(void) {
    exit(0);
}
