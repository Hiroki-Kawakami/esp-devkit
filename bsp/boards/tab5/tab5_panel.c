/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "tab5_panel.h"
#include "ili9881c.h"
#include "gt911.h"
#include "esp_log.h"

static const char *TAG = "tab5_panel";

#define TAB5_LCD_PIN_BL    GPIO_NUM_22
#define TAB5_TOUCH_PIN_INT GPIO_NUM_23

#define TAB5_PANEL_W       720
#define TAB5_PANEL_H       1280

static esp_err_t touch_init(const bsp_config_t *config, i2c_master_bus_handle_t bus) {
    /* tab5.c already ran the gtp_reset_guitar INT pulse (TP_RST sits on a PI4IOE
     * pin) and left INT high to latch the GT911 to 0x14. Pass reset_io=NC so the
     * driver doesn't try to re-reset; it just probes the chip on the bus. */
    const gt911_config_t cfg = {
        .i2c_bus     = bus,
        .i2c_address = GT911_I2C_ADDR_PRIMARY,
        .clock_hz    = GT911_I2C_DEFAULT_HZ,
        .int_io      = TAB5_TOUCH_PIN_INT,
        .reset_io    = GPIO_NUM_NC,
        .swap_xy     = false,
        .mirror_x    = false,
        .mirror_y    = false,
        .width       = TAB5_PANEL_W,
        .height      = TAB5_PANEL_H,
        .acquire = {
            .task_priority    = config->touch.task_priority,
            .task_affinity    = config->touch.task_affinity,
            .poll_interval_ms = config->touch.poll_interval_ms,
        },
    };
    bsp_touch_t *touch = NULL;
    esp_err_t err = gt911_touch_create(&cfg, &touch);
    if (err != ESP_OK) return err;
    bsp_touch_set_active(touch);
    return ESP_OK;
}

esp_err_t tab5_panel_init(const bsp_config_t *config, i2c_master_bus_handle_t i2c_bus) {
    const ili9881c_config_t cfg = {
        .size           = { TAB5_PANEL_W, TAB5_PANEL_H },
        .pixel_format   = BSP_PIXEL_FORMAT_RGB565,
        .fb_num         = 2,
        .backlight_gpio = TAB5_LCD_PIN_BL,
    };
    bsp_display_t *display = NULL;
    esp_err_t err = ili9881c_lcd_create(&cfg, &display);
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
