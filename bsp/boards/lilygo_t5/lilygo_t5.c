/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * LilyGo T5 ePaper S3: minimum board-side bsp_init orchestration.
 */

#include "bsp.h"
#include "bsp_dispatch.h"
#include "driver/i2c_master.h"
#include "esp_log.h"
#include "lilygo_t5_panel.h"

static const char *TAG = "lilygo_t5";

#define LILYGO_T5_I2C_PORT     I2C_NUM_0
#define LILYGO_T5_I2C_PIN_SDA  GPIO_NUM_18
#define LILYGO_T5_I2C_PIN_SCL  GPIO_NUM_17

static esp_err_t i2c_bus_init(i2c_master_bus_handle_t *out_bus) {
    const i2c_master_bus_config_t config = {
        .i2c_port          = LILYGO_T5_I2C_PORT,
        .sda_io_num        = LILYGO_T5_I2C_PIN_SDA,
        .scl_io_num        = LILYGO_T5_I2C_PIN_SCL,
        .clk_source        = I2C_CLK_SRC_DEFAULT,
        .glitch_ignore_cnt = 7,
        .flags.enable_internal_pullup = true,
    };
    esp_err_t err = i2c_new_master_bus(&config, out_bus);
    if (err != ESP_OK) ESP_LOGE(TAG, "i2c_new_master_bus: %s", esp_err_to_name(err));
    return err;
}

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_dispatch_configure(config ? config->dispatch.task_priority : 0,
                           config ? config->dispatch.task_affinity : -1);

    bsp_config_t defaults = {0};
    if (!config) config = &defaults;

    /* Touch is non-fatal: display bring-up remains usable if the I2C bus or
     * GT911 is unavailable. */
    i2c_master_bus_handle_t i2c_bus = NULL;
    (void)i2c_bus_init(&i2c_bus);
    return lilygo_t5_panel_init(config, i2c_bus);
}
