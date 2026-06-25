/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Paper (ESP32) panel bring-up: the IT8951E EPD on the shared SPI bus and the
 * GT911 touch on its own I2C bus.
 */

#include "paper_panel.h"
#include "paper_config.h"
#include "it8951e_epd.h"
#include "gt911.h"
#include "driver/i2c_master.h"
#include "esp_log.h"

static const char *TAG = "paper_panel";

static esp_err_t touch_init(const bsp_config_t *config) {
    const i2c_master_bus_config_t i2c_cfg = {
        .i2c_port          = PAPER_I2C_PORT,
        .sda_io_num        = PAPER_I2C_PIN_SDA,
        .scl_io_num        = PAPER_I2C_PIN_SCL,
        .clk_source        = I2C_CLK_SRC_DEFAULT,
        .glitch_ignore_cnt = 7,
        .flags.enable_internal_pullup = true,
    };
    i2c_master_bus_handle_t bus = NULL;
    esp_err_t err = i2c_new_master_bus(&i2c_cfg, &bus);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "i2c_new_master_bus: %s", esp_err_to_name(err));
        return err;
    }

    /* Same display geometry (960x540 landscape) as M5PaperS3, so the GT911 raw
     * portrait coordinates map to display space the same way: transpose, then
     * mirror the Y axis. Verify on hardware — the M5Paper digitizer mounting may
     * differ from the S3's. */
    const gt911_config_t cfg = {
        .i2c_bus     = bus,
        .i2c_address = GT911_I2C_ADDR_AUTO,
        .clock_hz    = GT911_I2C_DEFAULT_HZ,
        .int_io      = PAPER_TOUCH_PIN_INT,
        .reset_io    = PAPER_TOUCH_PIN_RST,
        .swap_xy     = true,
        .mirror_x    = false,
        .mirror_y    = true,
        .width       = 960,
        .height      = 540,
        .acquire = {
            .task_priority    = config->touch.task_priority,
            .task_affinity    = config->touch.task_affinity,
            .poll_interval_ms = config->touch.poll_interval_ms,
        },
    };
    bsp_touch_t *touch = NULL;
    err = gt911_touch_create(&cfg, &touch);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "gt911_touch_create: %s", esp_err_to_name(err));
        return err;
    }
    bsp_touch_set_active(touch);
    return ESP_OK;
}

esp_err_t paper_panel_init(const bsp_config_t *config) {
    const it8951e_config_t epd_cfg = {
        .spi_host = PAPER_SPI_HOST,
        .cs_io    = PAPER_EPD_PIN_CS,
        .busy_io  = PAPER_EPD_PIN_BUSY,
        .reset_io = GPIO_NUM_NC,
        .clock_hz = IT8951E_SPI_DEFAULT_HZ,
        .vcom_v   = PAPER_VCOM_VOLTS,
    };

    bsp_display_t *display = NULL;
    esp_err_t err = it8951e_epd_create(&epd_cfg, &display);
    if (err != ESP_OK) return err;
    bsp_display_set_active(display);

    /* Touch is non-fatal: a failure leaves bsp_touch_read a no-op rather than
     * blocking display bring-up. */
    err = touch_init(config);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "touch unavailable: %s", esp_err_to_name(err));
    }
    return ESP_OK;
}
