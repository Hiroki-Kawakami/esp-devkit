/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Core board entry: brings up the board and hands the display off to
 * core_panel. Host-side counterpart: core_sim.c.
 */

#include "bsp.h"
#include "esp_log.h"
#include "bsp_dispatch.h"
#include "core_panel.h"

static const char *TAG = "core";

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_dispatch_configure(config ? config->dispatch.task_priority : 0,
                           config ? config->dispatch.task_affinity : -1);

    esp_err_t err = core_panel_init();
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "core_panel_init: %s", esp_err_to_name(err));
        return err;
    }
    return ESP_OK;
}
