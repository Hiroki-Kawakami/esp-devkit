/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Paper (ESP32) board: device-side bsp_init orchestration. Sequences the
 * power rails, brings up the SPI bus shared by the IT8951E EPD and the microSD
 * card, then hands off to paper_panel_init (display + touch). The host-side
 * counterpart is paper_sim.c.
 */

#include "bsp.h"
#include "esp_system.h"
#include "esp_log.h"
#include "driver/gpio.h"
#include "driver/spi_master.h"
#include "paper_config.h"
#include "paper_panel.h"

static const char *TAG = "paper";

/* Raise the power rails in order; the IT8951E needs ~1 s to boot before it will
 * respond on SPI. */
static esp_err_t enable_power(void) {
    const gpio_config_t out = {
        .pin_bit_mask = (1ULL << PAPER_PIN_MAIN_PWR) |
                        (1ULL << PAPER_PIN_EXT_PWR_EN) |
                        (1ULL << PAPER_PIN_EPD_PWR_EN),
        .mode         = GPIO_MODE_OUTPUT,
        .pull_up_en   = GPIO_PULLUP_DISABLE,
        .pull_down_en = GPIO_PULLDOWN_DISABLE,
        .intr_type    = GPIO_INTR_DISABLE,
    };
    esp_err_t err = gpio_config(&out);
    if (err != ESP_OK) return err;

    gpio_set_level(PAPER_PIN_EPD_PWR_EN, 0);
    gpio_set_level(PAPER_PIN_EXT_PWR_EN, 0);
    gpio_set_level(PAPER_PIN_MAIN_PWR, 0);
    vTaskDelay(pdMS_TO_TICKS(100));

    gpio_set_level(PAPER_PIN_MAIN_PWR, 1);
    vTaskDelay(pdMS_TO_TICKS(100));
    gpio_set_level(PAPER_PIN_EXT_PWR_EN, 1);
    gpio_set_level(PAPER_PIN_EPD_PWR_EN, 1);
    vTaskDelay(pdMS_TO_TICKS(1000));
    return ESP_OK;
}

esp_err_t bsp_init(const bsp_config_t *config) {
    (void)config;   /* IT8951E refreshes synchronously: no async task to tune */

    esp_err_t err = enable_power();
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "enable_power: %s", esp_err_to_name(err));
        return err;
    }

    const spi_bus_config_t bus_cfg = {
        .mosi_io_num     = PAPER_SPI_PIN_MOSI,
        .miso_io_num     = PAPER_SPI_PIN_MISO,
        .sclk_io_num     = PAPER_SPI_PIN_SCK,
        .quadwp_io_num   = -1,
        .quadhd_io_num   = -1,
        .max_transfer_sz = 4096,
    };
    err = spi_bus_initialize(PAPER_SPI_HOST, &bus_cfg, SPI_DMA_CH_AUTO);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "spi_bus_initialize: %s", esp_err_to_name(err));
        return err;
    }

    return paper_panel_init();
}

void bsp_restart(void) {
    esp_restart();
}
