/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Air Quality Kit (ESP32-S3) board: device-side bsp_init. Brings up the
 * SPI bus for the GDEY0154D67 EPD and registers it as the active display, the
 * two front buttons (A=GPIO0, B=GPIO8) plus the power button (GPIO42, usable as
 * a normal button after boot), the passive buzzer on GPIO9, and the BM8563 RTC
 * on I2C (SCL=GPIO12, SDA=GPIO11). Power latches on GPIO46 (held high to stay on
 * when VBUS is absent) with battery sense on GPIO14. The host-side counterpart
 * is airq_sim.c.
 */

#include "bsp.h"
#include "bsp_audio.h"
#include "bsp_button.h"
#include "bsp_dispatch.h"
#include "esp_log.h"
#include "esp_system.h"
#include "driver/gpio.h"
#include "driver/spi_master.h"
#include "driver/i2c_master.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "bm8563.h"
#include "adc_battery.h"
#include "gdey0154d67_epd.h"
#include "gpio_button.h"
#include "pwm_buzzer.h"

static const char *TAG = "airq";

/* GDEY0154D67 on SPI2. MISO/BUSY-of-bus unused; CS is driven by the EPD driver. */
#define AIRQ_EPD_SPI_HOST   SPI2_HOST
#define AIRQ_EPD_PIN_MOSI   GPIO_NUM_6
#define AIRQ_EPD_PIN_SCLK   GPIO_NUM_5
#define AIRQ_EPD_PIN_CS     GPIO_NUM_4
#define AIRQ_EPD_PIN_DC     GPIO_NUM_3
#define AIRQ_EPD_PIN_RST    GPIO_NUM_2
#define AIRQ_EPD_PIN_BUSY   GPIO_NUM_1

#define AIRQ_BUTTON_A_GPIO   GPIO_NUM_0
#define AIRQ_BUTTON_B_GPIO   GPIO_NUM_8
#define AIRQ_BUTTON_PWR_GPIO GPIO_NUM_42

#define AIRQ_PIN_BUZZER     GPIO_NUM_9

/* Power latch: high holds the rail on when VBUS is absent (like M5Paper's MAIN_PWR).
 * Battery is 1/2 VBAT on GPIO14 (ADC2_CH3). */
#define AIRQ_PIN_MAIN_PWR   GPIO_NUM_46

#define AIRQ_I2C_PORT       I2C_NUM_0
#define AIRQ_I2C_PIN_SDA    GPIO_NUM_11
#define AIRQ_I2C_PIN_SCL    GPIO_NUM_12

static esp_err_t i2c_bus_init(i2c_master_bus_handle_t *out_bus) {
    const i2c_master_bus_config_t i2c_cfg = {
        .i2c_port          = AIRQ_I2C_PORT,
        .sda_io_num        = AIRQ_I2C_PIN_SDA,
        .scl_io_num        = AIRQ_I2C_PIN_SCL,
        .clk_source        = I2C_CLK_SRC_DEFAULT,
        .glitch_ignore_cnt = 7,
        .flags.enable_internal_pullup = true,
    };
    esp_err_t err = i2c_new_master_bus(&i2c_cfg, out_bus);
    if (err != ESP_OK) ESP_LOGE(TAG, "i2c_new_master_bus: %s", esp_err_to_name(err));
    return err;
}

/* BM8563 INT is not wired to the SoC (as on M5Paper), so leave int_io NC. */
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

static esp_err_t audio_init(const bsp_config_t *config) {
    const pwm_buzzer_config_t cfg = {
        .pwm_io  = AIRQ_PIN_BUZZER,
        .timer   = LEDC_TIMER_0,
        .channel = LEDC_CHANNEL_0,
    };
    bsp_audio_t *audio = NULL;
    esp_err_t err = pwm_buzzer_create(&cfg, &audio);
    if (err != ESP_OK) return err;
    bsp_audio_set_active(audio, &(bsp_audio_init_t){
        .dsp_mode = config->audio.dsp_mode,
        .speaker_mode = config->audio.speaker_mode,
    });
    return ESP_OK;
}

/* Hold the power latch on: VBUS keeps the rail alive on its own, but on battery
 * the board stays up only while GPIO46 is high. */
static void power_hold_init(void) {
    const gpio_config_t out = {
        .pin_bit_mask = 1ULL << AIRQ_PIN_MAIN_PWR,
        .mode         = GPIO_MODE_OUTPUT,
        .pull_up_en   = GPIO_PULLUP_DISABLE,
        .pull_down_en = GPIO_PULLDOWN_DISABLE,
        .intr_type    = GPIO_INTR_DISABLE,
    };
    gpio_config(&out);
    gpio_set_level(AIRQ_PIN_MAIN_PWR, 1);
}

/* Battery on GPIO14 (ADC2_CH3) via a 1:2 divider (read the pin ×2); 1S Li-ion
 * empty/full endpoints. No VBUS-sense GPIO. */
static void battery_init(void) {
    const adc_battery_config_t cfg = {
        .adc_unit    = ADC_UNIT_2,
        .adc_channel = ADC_CHANNEL_3,
        .adc_atten   = ADC_ATTEN_DB_12,
        .divider_mul = 2, .divider_div = 1,
        .empty_mv    = 3300, .full_mv = 4200,
        .vbus_gpio   = GPIO_NUM_NC,
    };
    bsp_power_t *power = NULL;
    if (adc_battery_create(&cfg, &power) == ESP_OK) bsp_power_set_active(power);
    else ESP_LOGW(TAG, "battery sense unavailable");
}

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_dispatch_configure(config ? config->dispatch.task_priority : 0,
                           config ? config->dispatch.task_affinity : -1);

    power_hold_init();

    const spi_bus_config_t bus_cfg = {
        .mosi_io_num     = AIRQ_EPD_PIN_MOSI,
        .miso_io_num     = -1,
        .sclk_io_num     = AIRQ_EPD_PIN_SCLK,
        .quadwp_io_num   = -1,
        .quadhd_io_num   = -1,
        .max_transfer_sz = 4096,
    };
    esp_err_t err = spi_bus_initialize(AIRQ_EPD_SPI_HOST, &bus_cfg, SPI_DMA_CH_AUTO);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "spi_bus_initialize: %s", esp_err_to_name(err));
        return err;
    }

    const gdey0154d67_epd_config_t epd_cfg = {
        .spi_host = AIRQ_EPD_SPI_HOST,
        .cs_io    = AIRQ_EPD_PIN_CS,
        .dc_io    = AIRQ_EPD_PIN_DC,
        .reset_io = AIRQ_EPD_PIN_RST,
        .busy_io  = AIRQ_EPD_PIN_BUSY,
    };
    bsp_display_t *display = NULL;
    err = gdey0154d67_epd_create(&epd_cfg, &display);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "gdey0154d67_epd_create: %s", esp_err_to_name(err));
        return err;
    }
    bsp_display_set_active(display);

    static const gpio_button_pin_t btn_pins[] = {
        { .gpio = AIRQ_BUTTON_A_GPIO, .active_low = true },
        { .gpio = AIRQ_BUTTON_B_GPIO, .active_low = true },
        { .gpio = AIRQ_BUTTON_PWR_GPIO, .active_low = true },
    };
    const gpio_button_config_t btn_cfg = {
        .pins        = btn_pins,
        .count       = 3,
        .enable_pull = true,
    };
    bsp_button_t *btn = NULL;
    if (gpio_button_create(&btn_cfg, &btn) == ESP_OK) {
        bsp_button_set_active(btn);
    } else {
        ESP_LOGW(TAG, "gpio_button unavailable");
    }

    if ((err = audio_init(config)) != ESP_OK) {
        ESP_LOGW(TAG, "audio unavailable: %s", esp_err_to_name(err));
    }

    /* Non-fatal: a failure leaves the bsp_rtc API a no-op. */
    i2c_master_bus_handle_t i2c_bus = NULL;
    if (i2c_bus_init(&i2c_bus) == ESP_OK && (err = rtc_init(i2c_bus)) != ESP_OK) {
        ESP_LOGW(TAG, "rtc unavailable: %s", esp_err_to_name(err));
    }

    battery_init();
    return ESP_OK;
}

void bsp_power_restart(void) {
    bsp_audio_quiesce();
    esp_restart();
}

esp_err_t bsp_power_hw_reset(void) {
    esp_err_t err = bsp_rtc_timer_start(200, false);
    if (err != ESP_OK) return err;
    return bsp_power_off();
}

esp_err_t bsp_power_off(void) {
    bsp_audio_quiesce();
    gpio_set_level(AIRQ_PIN_MAIN_PWR, 0);
    vTaskDelay(pdMS_TO_TICKS(500));
    gpio_set_level(AIRQ_PIN_MAIN_PWR, 1);
    return ESP_FAIL;
}
