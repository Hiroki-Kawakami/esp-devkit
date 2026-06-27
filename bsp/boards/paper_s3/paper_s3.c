/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5PaperS3 board: device-side bsp_init orchestration. Brings up the I2C bus
 * shared by the GT911 touch and the BM8563 RTC, hands the panel off to
 * paper_s3_panel_init (display + touch), and creates the RTC. The host-side
 * counterpart is paper_s3_sim.c.
 */

#include "bsp.h"
#include "esp_system.h"
#include "esp_log.h"
#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "bm8563.h"
#include "paper_s3_panel.h"

static const char *TAG = "paper_s3";

/* M5PaperS3 shared I2C bus (GT911 touch + BM8563 RTC). */
#define PAPER_S3_I2C_PORT     I2C_NUM_1
#define PAPER_S3_I2C_PIN_SDA  GPIO_NUM_41
#define PAPER_S3_I2C_PIN_SCL  GPIO_NUM_42

#define PAPER_S3_PIN_PWROFF_PULSE  GPIO_NUM_44

static esp_err_t i2c_bus_init(i2c_master_bus_handle_t *out_bus) {
    const i2c_master_bus_config_t i2c_cfg = {
        .i2c_port          = PAPER_S3_I2C_PORT,
        .sda_io_num        = PAPER_S3_I2C_PIN_SDA,
        .scl_io_num        = PAPER_S3_I2C_PIN_SCL,
        .clk_source        = I2C_CLK_SRC_DEFAULT,
        .glitch_ignore_cnt = 7,
        .flags.enable_internal_pullup = true,
    };
    esp_err_t err = i2c_new_master_bus(&i2c_cfg, out_bus);
    if (err != ESP_OK) ESP_LOGE(TAG, "i2c_new_master_bus: %s", esp_err_to_name(err));
    return err;
}

/* BM8563 INT is not wired to a readable GPIO (it gates the board power rail), so
 * leave int_io NC: the countdown timer's INT still asserts in hardware. */
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

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_config_t defaults = {0};   /* NULL/zeroed config -> EPD priority 5, core 0 */
    if (!config) config = &defaults;

    /* Touch + RTC share this I2C bus; both are non-fatal (a failure leaves their
     * bsp_* API a no-op rather than blocking display bring-up). */
    i2c_master_bus_handle_t i2c_bus = NULL;
    (void)i2c_bus_init(&i2c_bus);

    esp_err_t err = paper_s3_panel_init(config, i2c_bus);
    if (err != ESP_OK) return err;

    if (i2c_bus && (err = rtc_init(i2c_bus)) != ESP_OK) {
        ESP_LOGW(TAG, "rtc unavailable: %s", esp_err_to_name(err));
    }
    return ESP_OK;
}

void bsp_restart(void) {
    esp_restart();
}

esp_err_t bsp_power_off(void) {
    const gpio_config_t out = {
        .pin_bit_mask = 1ULL << PAPER_S3_PIN_PWROFF_PULSE,
        .mode         = GPIO_MODE_OUTPUT,
        .pull_up_en   = GPIO_PULLUP_DISABLE,
        .pull_down_en = GPIO_PULLDOWN_DISABLE,
        .intr_type    = GPIO_INTR_DISABLE,
    };
    gpio_config(&out);

    gpio_set_level(PAPER_S3_PIN_PWROFF_PULSE, 1);
    vTaskDelay(pdMS_TO_TICKS(500));
    gpio_set_level(PAPER_S3_PIN_PWROFF_PULSE, 0);
    return ESP_FAIL;
}
