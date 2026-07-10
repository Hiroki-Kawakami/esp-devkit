/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack CoreS3 panel bring-up: ILI9342C 320x240 on SPI2 (bus shared with the
 * microSD slot) + FT6336U I2C touch. Backlight is the AXP2101 DLDO1 rail; the LCD
 * RESET is AW9523B P1_1 (active low). The FT6336U INT is aggregated on AW9523B
 * P1_2 (not a native GPIO) and its RESET on AW9523B P0_0; core_s3.c owns the
 * expander INT -> touch wake, so touch reports int_io = BSP_TOUCH_INT_EXTERNAL.
 */

#include "core_s3_panel.h"
#include "ili9342c.h"
#include "ft6336u.h"
#include "bsp_display.h"
#include "bsp_touch.h"
#include "esp_log.h"
#include "driver/gpio.h"
#include "driver/spi_master.h"

static const char *TAG = "core_s3_panel";

#define LCD_SPI_HOST   SPI2_HOST
#define LCD_PIN_MOSI   GPIO_NUM_37
#define LCD_PIN_SCLK   GPIO_NUM_36
#define LCD_PIN_DC     GPIO_NUM_35
#define LCD_PIN_CS     GPIO_NUM_3
#define LCD_WIDTH      320
#define LCD_HEIGHT     240

#define AW_PIN_LCD_RST 9   /* AW9523B P1_1 -> 8 + 1 */

/* Backlight = AXP2101 DLDO1 voltage, 2.6-3.3 V over 0..100; 0 cuts the rail. */
static void backlight_set(void *ctx, int brightness) {
    axp2101_handle_t axp = ctx;
    if (brightness <= 0) {
        axp2101_set_rail_enabled(axp, AXP2101_DLDO1, false);
        return;
    }
    if (brightness > 100) brightness = 100;
    axp2101_set_rail_mv(axp, AXP2101_DLDO1, 2600 + (3300 - 2600) * brightness / 100);
    axp2101_set_rail_enabled(axp, AXP2101_DLDO1, true);
}

static void lcd_reset_set(void *ctx, bool asserted) {
    aw9523_set_output(ctx, AW_PIN_LCD_RST, !asserted);
}

esp_err_t core_s3_panel_init(i2c_master_bus_handle_t i2c_bus,
                             axp2101_handle_t axp, aw9523_t aw) {
    const spi_bus_config_t bus_cfg = {
        .mosi_io_num     = LCD_PIN_MOSI,
        .miso_io_num     = -1,
        .sclk_io_num     = LCD_PIN_SCLK,
        .quadwp_io_num   = -1,
        .quadhd_io_num   = -1,
        .max_transfer_sz = ILI9342C_DMA_CHUNK_BYTES,
    };
    esp_err_t err = spi_bus_initialize(LCD_SPI_HOST, &bus_cfg, SPI_DMA_CH_AUTO);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "spi_bus_initialize: %s", esp_err_to_name(err));
        return err;
    }

    const ili9342c_config_t cfg = {
        .spi_host        = LCD_SPI_HOST,
        .cs_io           = LCD_PIN_CS,
        .dc_io           = LCD_PIN_DC,
        .clock_hz        = ILI9342C_SPI_DEFAULT_HZ,
        .width           = LCD_WIDTH,
        .height          = LCD_HEIGHT,
        .madctl          = 0x08,
        .invert          = true,
        .set_reset       = lcd_reset_set,
        .reset_ctx       = aw,
        .set_backlight   = backlight_set,
        .backlight_ctx   = axp,
    };
    bsp_display_t *display = NULL;
    err = ili9342c_create(&cfg, &display);
    if (err != ESP_OK) return err;
    bsp_display_set_active(display);
    backlight_set(axp, 100);

    /* FT6336U on the system I2C bus; its INT is aggregated on the expander and
     * driven by core_s3.c, hence int_io = BSP_TOUCH_INT_EXTERNAL. */
    const ft6336u_config_t tp_cfg = {
        .i2c_bus  = i2c_bus,
        .int_io   = BSP_TOUCH_INT_EXTERNAL,
        .reset_io = GPIO_NUM_NC,
        .width    = LCD_WIDTH,
        .height   = LCD_HEIGHT,
    };
    bsp_touch_t *touch = NULL;
    if (ft6336u_touch_create(&tp_cfg, &touch) == ESP_OK) bsp_touch_set_active(touch);
    else ESP_LOGW(TAG, "touch unavailable");
    return ESP_OK;
}
