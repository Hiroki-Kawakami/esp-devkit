/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Core display bring-up. On Basic the ILI9342C 320x240 TFT hangs off
 * plain GPIOs: SPI2 (MOSI/SCLK/CS/DC), RESET on GPIO33 (active low), backlight
 * on GPIO32 driven by an LEDC PWM channel for brightness control.
 */

#include "core_panel.h"
#include "ili9342c.h"
#include "bsp_display.h"
#include "esp_log.h"
#include "esp_rom_sys.h"
#include "driver/gpio.h"
#include "driver/spi_master.h"
#include "driver/ledc.h"

static const char *TAG = "core_panel";

#define LCD_SPI_HOST   SPI2_HOST
#define LCD_PIN_MOSI   GPIO_NUM_23
#define LCD_PIN_SCLK   GPIO_NUM_18
#define LCD_PIN_CS     GPIO_NUM_14
#define LCD_PIN_DC     GPIO_NUM_27
#define LCD_PIN_RST    GPIO_NUM_33
#define LCD_PIN_BL     GPIO_NUM_32
#define LCD_WIDTH      320
#define LCD_HEIGHT     240

#define BL_LEDC_MODE   LEDC_LOW_SPEED_MODE
#define BL_LEDC_TIMER  LEDC_TIMER_0
#define BL_LEDC_CH     LEDC_CHANNEL_0
#define BL_LEDC_RES    LEDC_TIMER_10_BIT
#define BL_LEDC_HZ     5000
#define BL_DUTY_MAX    ((1u << 10) - 1)

static esp_err_t backlight_init(void) {
    const ledc_timer_config_t timer = {
        .speed_mode      = BL_LEDC_MODE,
        .duty_resolution = BL_LEDC_RES,
        .timer_num       = BL_LEDC_TIMER,
        .freq_hz         = BL_LEDC_HZ,
        .clk_cfg         = LEDC_AUTO_CLK,
    };
    esp_err_t err = ledc_timer_config(&timer);
    if (err != ESP_OK) return err;
    const ledc_channel_config_t ch = {
        .speed_mode = BL_LEDC_MODE,
        .channel    = BL_LEDC_CH,
        .timer_sel  = BL_LEDC_TIMER,
        .gpio_num   = LCD_PIN_BL,
        .duty       = 0,
        .hpoint     = 0,
    };
    return ledc_channel_config(&ch);
}

static void backlight_set(void *ctx, int brightness) {
    (void)ctx;
    if (brightness < 0) brightness = 0;
    if (brightness > 100) brightness = 100;
    ledc_set_duty(BL_LEDC_MODE, BL_LEDC_CH, (uint32_t)brightness * BL_DUTY_MAX / 100);
    ledc_update_duty(BL_LEDC_MODE, BL_LEDC_CH);
}

static void lcd_reset_set(void *ctx, bool asserted) {
    (void)ctx;
    gpio_set_level(LCD_PIN_RST, asserted ? 0 : 1);   /* RESET active low */
}

static bool panel_invert_probe(void) {
    gpio_reset_pin(LCD_PIN_RST);
    gpio_set_direction(LCD_PIN_RST, GPIO_MODE_OUTPUT);
    gpio_set_level(LCD_PIN_RST, 0);
    gpio_set_direction(LCD_PIN_RST, GPIO_MODE_INPUT);
    gpio_set_pull_mode(LCD_PIN_RST, GPIO_PULLDOWN_ONLY);
    esp_rom_delay_us(1000);
    bool invert = gpio_get_level(LCD_PIN_RST);
    gpio_set_pull_mode(LCD_PIN_RST, GPIO_FLOATING);
    gpio_set_direction(LCD_PIN_RST, GPIO_MODE_OUTPUT);
    gpio_set_level(LCD_PIN_RST, 1);
    return invert;
}

esp_err_t core_panel_init(void) {
    bool invert = panel_invert_probe();
    ESP_LOGI(TAG, "panel invert = %d (GPIO%d probe)", invert, LCD_PIN_RST);

    esp_err_t err;
    if ((err = backlight_init()) != ESP_OK) {
        ESP_LOGE(TAG, "backlight_init: %s", esp_err_to_name(err));
        return err;
    }

    const spi_bus_config_t bus_cfg = {
        .mosi_io_num     = LCD_PIN_MOSI,
        .miso_io_num     = -1,
        .sclk_io_num     = LCD_PIN_SCLK,
        .quadwp_io_num   = -1,
        .quadhd_io_num   = -1,
        .max_transfer_sz = ILI9342C_DMA_CHUNK_BYTES,
    };
    if ((err = spi_bus_initialize(LCD_SPI_HOST, &bus_cfg, SPI_DMA_CH_AUTO)) != ESP_OK) {
        ESP_LOGE(TAG, "spi_bus_initialize: %s", esp_err_to_name(err));
        return err;
    }

    const ili9342c_config_t cfg = {
        .spi_host      = LCD_SPI_HOST,
        .cs_io         = LCD_PIN_CS,
        .dc_io         = LCD_PIN_DC,
        .clock_hz      = ILI9342C_SPI_DEFAULT_HZ,
        .width         = LCD_WIDTH,
        .height        = LCD_HEIGHT,
        .madctl        = 0x08,
        .invert        = invert,
        .set_reset     = lcd_reset_set,
        .set_backlight = backlight_set,
    };
    bsp_display_t *display = NULL;
    if ((err = ili9342c_create(&cfg, &display)) != ESP_OK) {
        ESP_LOGE(TAG, "ili9342c_create: %s", esp_err_to_name(err));
        return err;
    }
    bsp_display_set_active(display);
    backlight_set(NULL, 100);
    return ESP_OK;
}
