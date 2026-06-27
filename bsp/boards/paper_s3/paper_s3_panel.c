/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "paper_s3_panel.h"
#include "ed047tc1.h"
#include "gt911.h"
#include "esp_log.h"

static const char *TAG = "paper_s3_panel";

/* GT911 INT is GPIO48 (output-capable so the driver can drive it during reset
 * and attach a data-ready ISR); RESET is on the on-module circuit, not exposed,
 * so the driver probes the address (0x14 then 0x5D). */
#define PAPER_S3_TOUCH_PIN_INT GPIO_NUM_48
#define PAPER_S3_TOUCH_PIN_RST GPIO_NUM_NC

static esp_err_t touch_init(const bsp_config_t *config, i2c_master_bus_handle_t bus) {
    /* The GT911 raw coordinate system is the panel's native portrait (X along
     * the 540 side, Y along the 960 side); the display is driven in landscape
     * 960x540, so transpose with swap_xy. The Y axis is mounted inverted
     * relative to the display, so mirror_y about the 540 height. */
    const gt911_config_t cfg = {
        .i2c_bus     = bus,
        .i2c_address = GT911_I2C_ADDR_AUTO,
        .clock_hz    = GT911_I2C_DEFAULT_HZ,
        .int_io      = PAPER_S3_TOUCH_PIN_INT,
        .reset_io    = PAPER_S3_TOUCH_PIN_RST,
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
    esp_err_t err = gt911_touch_create(&cfg, &touch);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "gt911_touch_create: %s", esp_err_to_name(err));
        return err;
    }
    bsp_touch_set_active(touch);
    return ESP_OK;
}

esp_err_t paper_s3_panel_init(const bsp_config_t *config, i2c_master_bus_handle_t i2c_bus) {
    ed047tc1_config_t cfg = {
        .data_pins     = { 6, 14, 7, 12, 9, 11, 8, 10 },
        .sph_pin       = 13,
        .cl_pin        = 16,
        .ckv_pin       = 18,
        .spv_pin       = 17,
        .le_pin        = 15,
        .oe_pin        = 45,
        .pwr_pin       = 46,
        .task_priority = config->epd.task_priority ? config->epd.task_priority : 5,
        .task_affinity = config->epd.task_affinity,
    };

    bsp_display_t *display = NULL;
    esp_err_t err = ed047tc1_epd_create(&cfg, &display);
    if (err != ESP_OK) return err;
    bsp_display_set_active(display);

    /* Touch is non-fatal: a failure leaves bsp_touch_read a no-op rather than
     * blocking display bring-up. */
    if (i2c_bus) {
        err = touch_init(config, i2c_bus);
        if (err != ESP_OK) ESP_LOGW(TAG, "touch unavailable: %s", esp_err_to_name(err));
    }
    return ESP_OK;
}
