/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "paper_s3_panel.h"
#include "ed047tc1.h"

esp_err_t paper_s3_panel_init(void) {
    ed047tc1_config_t cfg = {
        .data_pins    = { 6, 14, 7, 12, 9, 11, 8, 10 },
        .sph_pin      = 13,
        .cl_pin       = 16,
        .ckv_pin      = 18,
        .spv_pin      = 17,
        .le_pin       = 15,
        .oe_pin       = 45,
        .pwr_pin      = 46,
    };

    bsp_display_t *display = NULL;
    esp_err_t err = ed047tc1_epd_create(&cfg, &display);
    if (err != ESP_OK) return err;
    bsp_display_set_active(display);

    /* GT911 touch is not yet implemented; bsp_touch_read() is a no-op until a
     * provider is registered here. */
    return ESP_OK;
}
