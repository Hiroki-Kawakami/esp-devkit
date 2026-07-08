/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Tab5 panel bring-up. There are two panel generations shipping under
 * the same product name; pick by probing the touch chip on the shared I2C bus:
 *   - 0x55 -> ST7123 generation  (ST7123 MIPI LCD + ST7123 touch)
 *   - 0x14 -> ILI9881C generation (ILI9881C MIPI LCD + GT911 touch)
 * Both share the LCD_RST / TP_RST reset sequence done in tab5.c before this
 * runs, so we only have to create the display + touch providers.
 */

#include "tab5_panel.h"
#include "ili9881c.h"
#include "st7123_lcd.h"
#include "st7123_touch.h"
#include "gt911.h"
#include "esp_log.h"

static const char *TAG = "tab5_panel";

#define TAB5_LCD_PIN_BL    GPIO_NUM_22
#define TAB5_TOUCH_PIN_INT GPIO_NUM_23

#define TAB5_PANEL_W       720
#define TAB5_PANEL_H       1280

#define ST7123_TP_I2C_ADDR  0x55
#define GT911_TP_I2C_ADDR   0x14

static uint8_t resolve_fb_num(const bsp_config_t *config) {
    return config->display.fb_num ? config->display.fb_num : 2;
}

static bsp_pixel_format_t resolve_pixel_format(const bsp_config_t *config) {
    return config->display.pixel_format == BSP_PIXEL_FORMAT_RGB888
           ? BSP_PIXEL_FORMAT_RGB888 : BSP_PIXEL_FORMAT_RGB565;
}

static esp_err_t setup_st7123(const bsp_config_t *config, i2c_master_bus_handle_t bus) {
    const st7123_config_t lcd_cfg = {
        .size           = { TAB5_PANEL_W, TAB5_PANEL_H },
        .pixel_format   = resolve_pixel_format(config),
        .fb_num         = resolve_fb_num(config),
        .backlight_gpio = TAB5_LCD_PIN_BL,
    };
    bsp_display_t *display = NULL;
    esp_err_t err = st7123_lcd_create(&lcd_cfg, &display);
    if (err != ESP_OK) return err;
    bsp_display_set_active(display);

    const st7123_touch_config_t tp_cfg = {
        .i2c_bus     = bus,
        .clock_hz    = ST7123_I2C_DEFAULT_HZ,
        .int_io      = TAB5_TOUCH_PIN_INT,
        .reset_io    = GPIO_NUM_NC,
        .width       = TAB5_PANEL_W,
        .height      = TAB5_PANEL_H,
    };
    bsp_touch_t *touch = NULL;
    err = st7123_touch_create(&tp_cfg, &touch);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "st7123 touch unavailable: %s", esp_err_to_name(err));
        return ESP_OK;
    }
    bsp_touch_set_active(touch);
    return ESP_OK;
}

static esp_err_t setup_ili9881c(const bsp_config_t *config, i2c_master_bus_handle_t bus) {
    const ili9881c_config_t lcd_cfg = {
        .size           = { TAB5_PANEL_W, TAB5_PANEL_H },
        .pixel_format   = resolve_pixel_format(config),
        .fb_num         = resolve_fb_num(config),
        .backlight_gpio = TAB5_LCD_PIN_BL,
    };
    bsp_display_t *display = NULL;
    esp_err_t err = ili9881c_lcd_create(&lcd_cfg, &display);
    if (err != ESP_OK) return err;
    bsp_display_set_active(display);

    /* tab5.c already ran the gtp_reset_guitar INT pulse (TP_RST sits on a PI4IOE
     * pin) and left INT high to latch the GT911 to 0x14. Pass reset_io=NC so the
     * driver doesn't try to re-reset; it just probes the chip on the bus. */
    const gt911_config_t tp_cfg = {
        .i2c_bus     = bus,
        .i2c_address = GT911_I2C_ADDR_PRIMARY,
        .clock_hz    = GT911_I2C_DEFAULT_HZ,
        .int_io      = TAB5_TOUCH_PIN_INT,
        .reset_io    = GPIO_NUM_NC,
        .width       = TAB5_PANEL_W,
        .height      = TAB5_PANEL_H,
    };
    bsp_touch_t *touch = NULL;
    err = gt911_touch_create(&tp_cfg, &touch);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "gt911 touch unavailable: %s", esp_err_to_name(err));
        return ESP_OK;
    }
    bsp_touch_set_active(touch);
    return ESP_OK;
}

esp_err_t tab5_panel_init(const bsp_config_t *config, i2c_master_bus_handle_t i2c_bus) {
    if (!i2c_bus) return ESP_ERR_INVALID_ARG;

    if (i2c_master_probe(i2c_bus, ST7123_TP_I2C_ADDR, 10) == ESP_OK) {
        ESP_LOGI(TAG, "panel generation: ST7123");
        return setup_st7123(config, i2c_bus);
    }
    if (i2c_master_probe(i2c_bus, GT911_TP_I2C_ADDR, 10) == ESP_OK) {
        ESP_LOGI(TAG, "panel generation: ILI9881C");
        return setup_ili9881c(config, i2c_bus);
    }
    ESP_LOGE(TAG, "no known panel found on I2C (neither 0x%02x nor 0x%02x)",
             ST7123_TP_I2C_ADDR, GT911_TP_I2C_ADDR);
    return ESP_ERR_NOT_FOUND;
}
