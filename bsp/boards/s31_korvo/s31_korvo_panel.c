/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ESP32-S31-Korvo panel bring-up: 800x480 RGB565 parallel LCD via rgb_lcd.
 * Backlight and DISP_EN are not routed on this board (both wired NC in the
 * user guide); rgb_lcd no-ops the LEDC path when backlight_gpio == NC.
 * Pin map and timings taken from the vendor esp-dev-kits sample
 * (examples/esp32-s31-korvo/examples/common_components/esp32_s31_korvo).
 */

#include "s31_korvo_panel.h"
#include "rgb_lcd.h"
#include "gt1151.h"
#include "esp_log.h"

static const char *TAG = "s31_korvo_panel";

#define S31_LCD_W   800
#define S31_LCD_H   480

esp_err_t s31_korvo_panel_init(const bsp_config_t *config, i2c_master_bus_handle_t i2c_bus) {
    (void)config;
    const rgb_lcd_config_t lcd_cfg = {
        .size           = { S31_LCD_W, S31_LCD_H },
        .pixel_format   = BSP_PIXEL_FORMAT_RGB565,
        .pclk_hz        = 26 * 1000 * 1000,
        .data_width     = 16,
        .fb_num         = 2,
        .fb_in_psram    = true,
        .dma_burst_size = 128,
        .timing = {
            .hsync_pulse_width = 1,
            .hsync_back_porch  = 40,
            .hsync_front_porch = 20,
            .vsync_pulse_width = 1,
            .vsync_back_porch  = 10,
            .vsync_front_porch = 5,
            .pclk_active_neg   = true,
        },
        .data_gpios = {
            GPIO_NUM_8,  GPIO_NUM_9,  GPIO_NUM_10, GPIO_NUM_11,  /* B3..B6 */
            GPIO_NUM_12, GPIO_NUM_13, GPIO_NUM_14, GPIO_NUM_15,  /* B7, G2..G4 */
            GPIO_NUM_16, GPIO_NUM_17, GPIO_NUM_18, GPIO_NUM_19,  /* G5..G7, R3 */
            GPIO_NUM_33, GPIO_NUM_34, GPIO_NUM_35, GPIO_NUM_36,  /* R4..R7 */
        },
        .hsync_gpio     = GPIO_NUM_44,
        .vsync_gpio     = GPIO_NUM_45,
        .de_gpio        = GPIO_NUM_43,
        .pclk_gpio      = GPIO_NUM_40,
        .disp_gpio      = GPIO_NUM_NC,
        .backlight_gpio = GPIO_NUM_NC,
    };

    bsp_display_t *display = NULL;
    esp_err_t err = rgb_lcd_create(&lcd_cfg, &display);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "rgb_lcd_create: %s", esp_err_to_name(err));
        return err;
    }
    bsp_display_set_active(display);

    /* GT1151 touch. RST/INT are not wired on this board (both NC), so the
     * driver only probes the chip on its power-on I2C address. */
    const gt1151_config_t tp_cfg = {
        .i2c_bus     = i2c_bus,
        .i2c_address = GT1151_I2C_ADDR_PRIMARY,
        .clock_hz    = GT1151_I2C_DEFAULT_HZ,
        .int_io      = GPIO_NUM_NC,
        .reset_io    = GPIO_NUM_NC,
        .width       = S31_LCD_W,
        .height      = S31_LCD_H,
    };
    bsp_touch_t *touch = NULL;
    err = gt1151_touch_create(&tp_cfg, &touch);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "gt1151 touch unavailable: %s", esp_err_to_name(err));
        return ESP_OK;
    }
    bsp_touch_set_active(touch);
    return ESP_OK;
}
