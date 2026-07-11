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
#include "ft6336u.h"
#include "axp192.h"
#include "bsp_display.h"
#include "bsp_touch.h"
#include "bsp_button.h"
#include "gpio_button.h"
#include "esp_log.h"
#include "esp_rom_sys.h"
#include "driver/gpio.h"
#include "driver/spi_master.h"
#include "driver/ledc.h"

static const char *TAG = "core_panel";

#define LCD_SPI_HOST      SPI2_HOST
#define LCD_PIN_MOSI      GPIO_NUM_23
#define LCD_PIN_SCLK      GPIO_NUM_18
#define LCD_PIN_CS_BASIC  GPIO_NUM_14
#define LCD_PIN_DC_BASIC  GPIO_NUM_27
#define LCD_PIN_CS_CORE2  GPIO_NUM_5
#define LCD_PIN_DC_CORE2  GPIO_NUM_15
#define LCD_PIN_RST       GPIO_NUM_33
#define LCD_PIN_BL        GPIO_NUM_32
#define LCD_WIDTH         320
#define LCD_HEIGHT        240

#define TOUCH_PIN_INT     GPIO_NUM_39
#define TOUCH_BTN_Y0      240
#define TOUCH_BTN_Y1      280

#define BTN_PIN_A         GPIO_NUM_39
#define BTN_PIN_B         GPIO_NUM_38
#define BTN_PIN_C         GPIO_NUM_37

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

#define AXP_REG_GPIO34_FUNC  0x95
#define AXP_REG_GPIO34_LVL   0x96
#define AXP_GPIO4_FUNC_KEEP  0x72
#define AXP_GPIO4_NMOS_OUT   0x84
#define AXP_GPIO4_LVL_BIT    0x02

static void axp_lcd_reset_init(axp192_handle_t axp) {
    uint8_t func = 0;
    axp192_read_reg(axp, AXP_REG_GPIO34_FUNC, &func);
    axp192_write_reg(axp, AXP_REG_GPIO34_FUNC, (func & AXP_GPIO4_FUNC_KEEP) | AXP_GPIO4_NMOS_OUT);
}

static void axp_lcd_reset_set(void *ctx, bool asserted) {
    axp192_handle_t axp = ctx;
    uint8_t lvl = 0;
    axp192_read_reg(axp, AXP_REG_GPIO34_LVL, &lvl);
    if (asserted) lvl &= ~AXP_GPIO4_LVL_BIT;
    else          lvl |= AXP_GPIO4_LVL_BIT;
    axp192_write_reg(axp, AXP_REG_GPIO34_LVL, lvl);
}

static void axp_backlight_set(void *ctx, int brightness) {
    axp192_handle_t axp = ctx;
    if (brightness <= 0) {
        axp192_set_rail_enabled(axp, AXP192_RAIL_DCDC3, false);
        return;
    }
    if (brightness > 100) brightness = 100;
    axp192_set_dcdc3_mv(axp, 2500 + (3300 - 2500) * brightness / 100);
    axp192_set_rail_enabled(axp, AXP192_RAIL_DCDC3, true);
}

static void axp_panel_power_set(void *ctx, bool on) {
    axp192_set_rail_enabled(ctx, AXP192_RAIL_LDO2, on);
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

static void core_input_init(i2c_master_bus_handle_t i2c_bus) {
    if (i2c_master_probe(i2c_bus, FT6336U_I2C_ADDR, 100) == ESP_OK) {
        const ft6336u_config_t tp_cfg = {
            .i2c_bus  = i2c_bus,
            .int_io   = GPIO_NUM_NC,
            .reset_io = GPIO_NUM_NC,
            .width    = LCD_WIDTH,
            .height   = LCD_HEIGHT,
        };
        bsp_touch_t *touch = NULL;
        esp_err_t err = ft6336u_touch_create(&tp_cfg, &touch);
        if (err != ESP_OK) {
            ESP_LOGW(TAG, "touch unavailable: %s", esp_err_to_name(err));
            return;
        }
        bsp_touch_set_active(touch);

        static const bsp_touch_zone_t zones[] = {
            {   0, TOUCH_BTN_Y0, 106, TOUCH_BTN_Y1 },
            { 107, TOUCH_BTN_Y0, 213, TOUCH_BTN_Y1 },
            { 214, TOUCH_BTN_Y0, 320, TOUCH_BTN_Y1 },
        };
        bsp_touch_set_button(&(bsp_touch_button_config_t){ .zones = zones, .count = 3 });
    } else {
        static const gpio_button_pin_t pins[] = {
            { BTN_PIN_A, true },
            { BTN_PIN_B, true },
            { BTN_PIN_C, true },
        };
        const gpio_button_config_t gcfg = { .pins = pins, .count = 3, .enable_pull = false };
        bsp_button_raw_t *btn = NULL;
        if (gpio_button_create(&gcfg, &btn) == ESP_OK) bsp_button_add_raw(btn);
        else ESP_LOGW(TAG, "gpio buttons unavailable");
    }
}

esp_err_t core_panel_init(i2c_master_bus_handle_t i2c_bus, axp192_handle_t axp) {
    esp_err_t err;
    bool invert;
    if (axp) {
        invert = true;
        axp192_set_ldo2_mv(axp, 3300);
        axp192_set_rail_enabled(axp, AXP192_RAIL_LDO2, true);
        axp_lcd_reset_init(axp);
    } else {
        invert = panel_invert_probe();
        ESP_LOGI(TAG, "panel invert = %d (GPIO%d probe)", invert, LCD_PIN_RST);
        if ((err = backlight_init()) != ESP_OK) {
            ESP_LOGE(TAG, "backlight_init: %s", esp_err_to_name(err));
            return err;
        }
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
        .spi_host        = LCD_SPI_HOST,
        .cs_io           = axp ? LCD_PIN_CS_CORE2 : LCD_PIN_CS_BASIC,
        .dc_io           = axp ? LCD_PIN_DC_CORE2 : LCD_PIN_DC_BASIC,
        .clock_hz        = ILI9342C_SPI_DEFAULT_HZ,
        .width           = LCD_WIDTH,
        .height          = LCD_HEIGHT,
        .madctl          = 0x08,
        .invert          = invert,
        .set_reset       = axp ? axp_lcd_reset_set : lcd_reset_set,
        .reset_ctx       = axp,
        .set_backlight   = axp ? axp_backlight_set : backlight_set,
        .backlight_ctx   = axp,
        .set_panel_power = axp ? axp_panel_power_set : NULL,
        .panel_power_ctx = axp,
    };
    bsp_display_t *display = NULL;
    if ((err = ili9342c_create(&cfg, &display)) != ESP_OK) {
        ESP_LOGE(TAG, "ili9342c_create: %s", esp_err_to_name(err));
        return err;
    }
    bsp_display_set_active(display);
    cfg.set_backlight(axp, 100);

    core_input_init(i2c_bus);
    return ESP_OK;
}
