/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Paper (ESP32) board: device-side bsp_init orchestration. Sequences the
 * power rails, brings up the SPI bus shared by the IT8951E EPD and the microSD
 * card and the I2C bus shared by the GT911 touch and the BM8563 RTC, hands the
 * panel off to paper_panel_init (display + touch), and creates the RTC. The
 * host-side counterpart is paper_sim.c.
 */

#include "bsp.h"
#include "esp_system.h"
#include "esp_log.h"
#include "driver/gpio.h"
#include "driver/spi_master.h"
#include "driver/i2c_master.h"
#include "bm8563.h"
#include "gpio_button.h"
#include "adc_battery.h"
#include "paper_config.h"
#include "paper_panel.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

static const char *TAG = "paper";

static esp_err_t i2c_bus_init(i2c_master_bus_handle_t *out_bus) {
    const i2c_master_bus_config_t i2c_cfg = {
        .i2c_port          = PAPER_I2C_PORT,
        .sda_io_num        = PAPER_I2C_PIN_SDA,
        .scl_io_num        = PAPER_I2C_PIN_SCL,
        .clk_source        = I2C_CLK_SRC_DEFAULT,
        .glitch_ignore_cnt = 7,
        .flags.enable_internal_pullup = true,
    };
    esp_err_t err = i2c_new_master_bus(&i2c_cfg, out_bus);
    if (err != ESP_OK) ESP_LOGE(TAG, "i2c_new_master_bus: %s", esp_err_to_name(err));
    return err;
}

/* BM8563 INT gates the M5Paper power latch (not a readable GPIO), so leave int_io
 * NC: the countdown timer's INT still asserts in hardware. */
static esp_err_t rtc_init(i2c_master_bus_handle_t bus) {
    const bm8563_config_t cfg = {
        .i2c_bus     = bus,
        .i2c_address = BM8563_I2C_ADDR,
        .clock_hz    = BM8563_I2C_DEFAULT_HZ,
        .int_io      = GPIO_NUM_NC,
    };
    bsp_rtc_t *rtc = NULL;
    esp_err_t err = bm8563_rtc_create(&cfg, &rtc);
    if (err != ESP_OK) return err;
    bsp_rtc_set_active(rtc);
    return ESP_OK;
}

/* Three side keys on input-only pins with external pull-ups (active-low). */
static void buttons_init(void) {
    static const gpio_button_pin_t pins[] = {
        { PAPER_BTN_PIN_PUSH, true },
        { PAPER_BTN_PIN_UP  , true },
        { PAPER_BTN_PIN_DOWN, true },
    };
    const gpio_button_config_t cfg = {
        .pins  = pins,
        .count = sizeof(pins) / sizeof(pins[0]),
    };
    bsp_button_t *btn = NULL;
    esp_err_t err = gpio_button_create(&cfg, &btn);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "gpio_button_create: %s", esp_err_to_name(err));
        return;
    }
    bsp_button_set_active(btn);
}

/* Battery on GPIO35 (ADC1_CH7) via a 1:1 divider (read the pin ×2); 1S Li-ion
 * empty/full endpoints. No VBUS-sense GPIO. */
static void battery_init(void) {
    const adc_battery_config_t cfg = {
        .adc_unit    = ADC_UNIT_1,
        .adc_channel = ADC_CHANNEL_7,
        .adc_atten   = ADC_ATTEN_DB_12,
        .divider_mul = 2, .divider_div = 1,
        .empty_mv    = 3300, .full_mv = 4200,
        .vbus_gpio   = GPIO_NUM_NC,
    };
    bsp_power_t *power = NULL;
    if (adc_battery_create(&cfg, &power) == ESP_OK) bsp_power_set_active(power);
    else ESP_LOGW(TAG, "battery sense unavailable");
}

/* Raise the power rails in order; the IT8951E needs ~1 s to boot before it will
 * respond on SPI. */
static esp_err_t enable_power(void) {
    const gpio_config_t out = {
        .pin_bit_mask = (1ULL << PAPER_PIN_MAIN_PWR) |
                        (1ULL << PAPER_PIN_EXT_PWR_EN) |
                        (1ULL << PAPER_PIN_EPD_PWR_EN),
        .mode         = GPIO_MODE_OUTPUT,
        .pull_up_en   = GPIO_PULLUP_DISABLE,
        .pull_down_en = GPIO_PULLDOWN_DISABLE,
        .intr_type    = GPIO_INTR_DISABLE,
    };
    esp_err_t err = gpio_config(&out);
    if (err != ESP_OK) return err;

    gpio_set_level(PAPER_PIN_EPD_PWR_EN, 0);
    gpio_set_level(PAPER_PIN_EXT_PWR_EN, 0);
    gpio_set_level(PAPER_PIN_MAIN_PWR, 0);
    vTaskDelay(pdMS_TO_TICKS(100));

    gpio_set_level(PAPER_PIN_MAIN_PWR, 1);
    gpio_set_level(PAPER_PIN_EXT_PWR_EN, 1);
    gpio_set_level(PAPER_PIN_EPD_PWR_EN, 1);
    return ESP_OK;
}

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_config_t defaults = {0};   /* IT8951E refreshes synchronously: no EPD task */
    if (!config) config = &defaults;

    esp_err_t err = enable_power();
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "enable_power: %s", esp_err_to_name(err));
        return err;
    }

    const spi_bus_config_t bus_cfg = {
        .mosi_io_num     = PAPER_SPI_PIN_MOSI,
        .miso_io_num     = PAPER_SPI_PIN_MISO,
        .sclk_io_num     = PAPER_SPI_PIN_SCK,
        .quadwp_io_num   = -1,
        .quadhd_io_num   = -1,
        .max_transfer_sz = 4096,
    };
    err = spi_bus_initialize(PAPER_SPI_HOST, &bus_cfg, SPI_DMA_CH_AUTO);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "spi_bus_initialize: %s", esp_err_to_name(err));
        return err;
    }

    /* Touch + RTC share this I2C bus; both are non-fatal (a failure leaves their
     * bsp_* API a no-op rather than blocking display bring-up). */
    i2c_master_bus_handle_t i2c_bus = NULL;
    (void)i2c_bus_init(&i2c_bus);

    err = paper_panel_init(config, i2c_bus);
    if (err != ESP_OK) return err;

    if (i2c_bus && (err = rtc_init(i2c_bus)) != ESP_OK) {
        ESP_LOGW(TAG, "rtc unavailable: %s", esp_err_to_name(err));
    }

    buttons_init();
    battery_init();
    return ESP_OK;
}

void bsp_power_restart(void) {
    esp_restart();
}

esp_err_t bsp_power_hw_reset(void) {
    esp_err_t err = bsp_rtc_timer_start(200, false);
    if (err != ESP_OK) return err;
    return bsp_power_off();
}

esp_err_t bsp_power_off(void) {
    gpio_set_level(PAPER_PIN_MAIN_PWR, 0);
    vTaskDelay(pdMS_TO_TICKS(500));
    gpio_set_level(PAPER_PIN_MAIN_PWR, 1);
    return ESP_FAIL;
}
