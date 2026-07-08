/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Air Quality Kit (ESP32-S3) board: device-side bsp_init. Brings up the
 * SPI bus for the GDEY0154D67 EPD and registers it as the active display. Power
 * controls fall back to the shared defaults (USB-powered, esp_restart). The
 * host-side counterpart is airq_sim.c.
 */

#include "bsp.h"
#include "esp_log.h"
#include "driver/spi_master.h"
#include "gdey0154d67_epd.h"

static const char *TAG = "airq";

/* GDEY0154D67 on SPI2. MISO/BUSY-of-bus unused; CS is driven by the EPD driver. */
#define AIRQ_EPD_SPI_HOST   SPI2_HOST
#define AIRQ_EPD_PIN_MOSI   GPIO_NUM_6
#define AIRQ_EPD_PIN_SCLK   GPIO_NUM_5
#define AIRQ_EPD_PIN_CS     GPIO_NUM_4
#define AIRQ_EPD_PIN_DC     GPIO_NUM_3
#define AIRQ_EPD_PIN_RST    GPIO_NUM_2
#define AIRQ_EPD_PIN_BUSY   GPIO_NUM_1

esp_err_t bsp_init(const bsp_config_t *config) {
    (void)config;

    const spi_bus_config_t bus_cfg = {
        .mosi_io_num     = AIRQ_EPD_PIN_MOSI,
        .miso_io_num     = -1,
        .sclk_io_num     = AIRQ_EPD_PIN_SCLK,
        .quadwp_io_num   = -1,
        .quadhd_io_num   = -1,
        .max_transfer_sz = 4096,
    };
    esp_err_t err = spi_bus_initialize(AIRQ_EPD_SPI_HOST, &bus_cfg, SPI_DMA_CH_AUTO);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "spi_bus_initialize: %s", esp_err_to_name(err));
        return err;
    }

    const gdey0154d67_epd_config_t epd_cfg = {
        .spi_host = AIRQ_EPD_SPI_HOST,
        .cs_io    = AIRQ_EPD_PIN_CS,
        .dc_io    = AIRQ_EPD_PIN_DC,
        .reset_io = AIRQ_EPD_PIN_RST,
        .busy_io  = AIRQ_EPD_PIN_BUSY,
    };
    bsp_display_t *display = NULL;
    err = gdey0154d67_epd_create(&epd_cfg, &display);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "gdey0154d67_epd_create: %s", esp_err_to_name(err));
        return err;
    }
    bsp_display_set_active(display);

    return ESP_OK;
}
