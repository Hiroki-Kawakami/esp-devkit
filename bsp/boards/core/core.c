/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Core board entry: brings up the board and hands the display off to
 * core_panel. Host-side counterpart: core_sim.c.
 */

#include "bsp.h"
#include "esp_log.h"
#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "bsp_dispatch.h"
#include "core_panel.h"
#include "axp192.h"

static const char *TAG = "core";

#define I2C_PORT       I2C_NUM_0
#define I2C_PIN_SDA    GPIO_NUM_21
#define I2C_PIN_SCL    GPIO_NUM_22

static esp_err_t i2c_bus_init(i2c_master_bus_handle_t *out_bus) {
    const i2c_master_bus_config_t cfg = {
        .i2c_port          = I2C_PORT,
        .sda_io_num        = I2C_PIN_SDA,
        .scl_io_num        = I2C_PIN_SCL,
        .clk_source        = I2C_CLK_SRC_DEFAULT,
        .glitch_ignore_cnt = 7,
        .flags.enable_internal_pullup = true,
    };
    esp_err_t err = i2c_new_master_bus(&cfg, out_bus);
    if (err != ESP_OK) ESP_LOGE(TAG, "i2c_new_master_bus: %s", esp_err_to_name(err));
    return err;
}

static axp192_handle_t axp_probe(i2c_master_bus_handle_t bus) {
    if (i2c_master_probe(bus, AXP192_I2C_ADDR, 100) != ESP_OK) return NULL;
    const axp192_config_t cfg = {
        .i2c_bus     = bus,
        .i2c_address = AXP192_I2C_ADDR,
        .clock_hz    = AXP192_I2C_DEFAULT_HZ,
    };
    axp192_handle_t axp = NULL;
    esp_err_t err = axp192_create(&cfg, &axp);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "axp192_create: %s", esp_err_to_name(err));
        return NULL;
    }
    return axp;
}

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_dispatch_configure(config ? config->dispatch.task_priority : 0,
                           config ? config->dispatch.task_affinity : -1);

    i2c_master_bus_handle_t i2c_bus = NULL;
    esp_err_t err = i2c_bus_init(&i2c_bus);
    if (err != ESP_OK) return err;

    axp192_handle_t axp = axp_probe(i2c_bus);
    ESP_LOGI(TAG, "board: %s", axp ? "Core2" : "Basic");

    if ((err = core_panel_init(i2c_bus, axp)) != ESP_OK) {
        ESP_LOGE(TAG, "core_panel_init: %s", esp_err_to_name(err));
        return err;
    }
    return ESP_OK;
}
