/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Paper (ESP32) simulator board: the host-side counterpart of paper.c. The
 * SDL backend mimics the EPD; the panel geometry matches M5PaperS3 so the shared
 * app renders identically. Build with -DBSP_BOARD=paper.
 */

#include "bsp.h"
#include "bsp_display.h"
#include "bsp_touch.h"
#include "bsp_dispatch.h"
#include "bsp_rtc.h"
#include "bsp_power.h"
#include "sdl_panel.h"
#include "rtc_sim.h"
#include "power_sim.h"
#include <stdio.h>

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_dispatch_configure(config ? config->dispatch.task_priority : 0,
                           config ? config->dispatch.task_affinity : -1);
    sdl_panel_config_t sdl_config = {
        .title     = "M5Paper",
        .type      = BSP_DISPLAY_TYPE_SPI_EPD,
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

    bsp_power_t *power = NULL;
    if (power_sim_create(&power) == ESP_OK) bsp_power_set_active(power);
    return ESP_OK;
}

/* No PMIC on the host: behave like a USB-powered device that stays alive. */
esp_err_t bsp_power_off(void) {
    fprintf(stderr, "[sim] bsp_power_off: staying on\n");
    return ESP_FAIL;
}
