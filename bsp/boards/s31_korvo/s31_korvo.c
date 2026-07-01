/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ESP32-S31-Korvo board (ESP32-S31 + 800x480 RGB parallel LCD). Minimum
 * bring-up: hands off to s31_korvo_panel_init for the RGB LCD. I2C bus (SDA=0,
 * SCL=1) and GT1151 touch will be added when touch support lands.
 */

#include "bsp.h"
#include "esp_system.h"
#include "s31_korvo_panel.h"

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_config_t defaults = {0};
    if (!config) config = &defaults;
    return s31_korvo_panel_init(config);
}

void bsp_restart(void) {
    esp_restart();
}

esp_err_t bsp_hw_reset(void) {
    esp_restart();
    return ESP_FAIL;
}

esp_err_t bsp_power_off(void) {
    return ESP_ERR_NOT_SUPPORTED;
}
