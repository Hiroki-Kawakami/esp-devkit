/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5PaperS3 board: device-side bsp_init orchestration. Brings up the panel
 * (display + touch); the host-side counterpart is paper_s3_sim.c.
 */

#include "bsp.h"
#include "esp_system.h"
#include "paper_s3_panel.h"

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_config_t defaults = {0};   /* NULL/zeroed config -> EPD priority 5, core 0 */
    if (!config) config = &defaults;
    return paper_s3_panel_init(config);
}

void bsp_restart(void) {
    esp_restart();
}
