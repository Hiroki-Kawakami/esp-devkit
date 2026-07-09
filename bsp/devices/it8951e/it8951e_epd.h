/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * IT8951E EPD display provider: wraps the low-level it8951e SPI TCON driver
 * (it8951e.h) into a BSP display vtable (bsp_display_t). This owns the handle,
 * keeps a panel-sized L8 GRAM, and implements draw_bitmap / set_epd_mode /
 * refresh on top of it8951e_load_image + it8951e_display.
 *
 * Power model (set_power): the TCON auto-gates to STANDBY between refreshes
 * (each refresh does SYS_RUN, drives, then STANDBY). set_power adds SLEEP (deep
 * sleep) and OFF (cut the panel rail via set_panel_power; NULL -> OFF collapses
 * to SLEEP). Waking from SLEEP/OFF re-inits the TCON on the next refresh.
 */

#pragma once
#include "bsp_display.h"
#include "it8951e.h"

/* EPD rail control (NULL -> POWER_OFF collapses to POWER_SLEEP). */
typedef void (*it8951e_epd_power_cb_t)(void *ctx, bool on);

typedef struct {
    it8951e_config_t       tcon;
    it8951e_epd_power_cb_t set_panel_power;
    void                  *panel_power_ctx;
} it8951e_epd_config_t;

esp_err_t it8951e_epd_create(const it8951e_epd_config_t *cfg, bsp_display_t **out_display);
