/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5PaperS3 simulator board: the host-side counterpart of paper_s3.c.
 */

#include "bsp.h"
#include "bsp_display.h"
#include "bsp_touch.h"
#include "bsp_rtc.h"
#include "sdl_panel.h"
#include "rtc_sim.h"

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

    bsp_rtc_t *rtc = NULL;
    if (rtc_sim_create(&rtc) == ESP_OK) bsp_rtc_set_active(rtc);
    return ESP_OK;
}

void bsp_restart(void) {
    exit(0);
}

esp_err_t bsp_hw_reset(void) {
    exit(0);
}

esp_err_t bsp_power_off(void) {
    exit(0);
}
