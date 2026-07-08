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
#include "gt911_hotknot.h"
#include "esp_log.h"

static const char *TAG = "paper_panel";

static esp_err_t touch_init(i2c_master_bus_handle_t bus) {
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
        /* HotKnot SNR tuning: same battery-noise profile as M5PaperS3. M5Paper's
         * INT is input-only (RESET also unwired), so a session leaves touch needing
         * a power cycle — see bsp_hotknot_end docs. */
        .hotknot = {
            .noise_map     = 0x3F,
            .pxy_threshold = 0x14,
            .dump_shift    = 0x02,
            .rx_gain       = 0x03,
            .freq_gain     = 0x88,
        },
    };
    bsp_touch_t *touch = NULL;
    esp_err_t err = gt911_touch_create(&cfg, &touch);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "gt911_touch_create: %s", esp_err_to_name(err));
        return err;
    }
    bsp_touch_set_active(touch);

    /* HotKnot needs the dispatch-driven touch panel; non-fatal if unavailable. */
    bsp_hotknot_t *hk = NULL;
    if (gt911_hotknot_create(&hk) == ESP_OK) bsp_hotknot_set_active(hk);
    return ESP_OK;
}

esp_err_t paper_panel_init(const bsp_config_t *config, i2c_master_bus_handle_t i2c_bus) {
    (void)config;
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
    if (i2c_bus) {
        err = touch_init(i2c_bus);
        if (err != ESP_OK) ESP_LOGW(TAG, "touch unavailable: %s", esp_err_to_name(err));
    }
    return ESP_OK;
}
