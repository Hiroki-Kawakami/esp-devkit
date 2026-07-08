/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * AtomS3 Lite simulator board. The device has no display and the LED/button
 * are physical-only, so there is nothing to render: bsp_init is a no-op and the
 * LED/button providers stay unregistered (bsp_led_count/bsp_button_count == 0).
 */

#include "bsp.h"
#include "bsp_dispatch.h"

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_dispatch_configure(config ? config->dispatch.task_priority : 0,
                           config ? config->dispatch.task_affinity : -1);
    return ESP_OK;
}
