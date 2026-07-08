/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Air Quality Kit simulator board: host-side counterpart of airq.c. The
 * SDL backend mimics the 200x200 B/W EPD so the shared app renders identically.
 * Display-only: the front buttons are physical-only, so they stay unwired here;
 * power controls fall back to the shared defaults.
 * Build with -DBSP_BOARD=airq.
 */

#include "bsp.h"
#include "bsp_display.h"
#include "bsp_dispatch.h"
#include "sdl_panel.h"

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_dispatch_configure(config ? config->dispatch.task_priority : 0,
                           config ? config->dispatch.task_affinity : -1);
    sdl_panel_config_t sdl_config = {
        .title     = "M5 AirQ",
        .type      = BSP_DISPLAY_TYPE_SPI_EPD,
        .size      = { 200, 200 },
        .format    = BSP_PIXEL_FORMAT_L8,
        .scale_div = 1,
    };

    bsp_display_t *display = NULL;
    esp_err_t err = sdl_panel_create(&sdl_config, &display, NULL);
    if (err != ESP_OK) return err;

    bsp_display_set_active(display);
    return ESP_OK;
}
