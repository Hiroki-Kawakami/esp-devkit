/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5StickC-Plus (ESP32-PICO-D4) board: device-side bsp_init orchestration.
 * Brings up the I2C bus shared by the AXP192 PMIC and the BM8563 RTC (and, later,
 * the MPU6886 IMU), raises the TFT rails through the AXP192 (LDO3 = panel IC,
 * LDO2 = backlight), then the SPI bus for the ST7789V2 panel. The AXP192 also
 * backs the power-sensing seam; the BM8563 countdown backs bsp_power_hw_reset.
 * The passive buzzer on GPIO2 is a tone-only bsp_audio provider; the red LED on
 * GPIO10 is a PWM bsp_led provider. The host-side counterpart is
 * stickc_plus_sim.c.
 *
 * Pin map from the M5StickC-Plus schematic. Display / buttons / buzzer / LED /
 * RTC are wired (IMU comes later).
 */

#include "bsp.h"
#include "esp_system.h"
#include "esp_log.h"
#include "driver/gpio.h"
#include "driver/spi_master.h"
#include "driver/i2c_master.h"
#include "axp192.h"
#include "st7789v2.h"
#include "bm8563.h"
#include "gpio_button.h"
#include "pwm_buzzer.h"
#include "pwm_led.h"
#include "bsp_audio.h"
#include "bsp_button.h"
#include "bsp_led.h"
#include "bsp_rtc.h"
#include "bsp_dispatch.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

static const char *TAG = "stickc_plus";

/* I2C: AXP192 (+ BM8563 RTC, MPU6886 IMU). */
#define I2C_PORT       I2C_NUM_0
#define I2C_PIN_SDA    GPIO_NUM_21
#define I2C_PIN_SCL    GPIO_NUM_22

/* ST7789V2 TFT on SPI2. */
#define LCD_SPI_HOST   SPI2_HOST
#define LCD_PIN_MOSI   GPIO_NUM_15
#define LCD_PIN_SCLK   GPIO_NUM_13
#define LCD_PIN_DC     GPIO_NUM_23
#define LCD_PIN_RST    GPIO_NUM_18
#define LCD_PIN_CS     GPIO_NUM_5

/* 135x240 glass centered in the ST7789's 240x320 GRAM. */
#define LCD_WIDTH      135
#define LCD_HEIGHT     240
#define LCD_X_OFFSET   52
#define LCD_Y_OFFSET   40

/* Buttons: A/B on plain GPIOs (active-low, external pull-ups) added first, then
 * the AXP192 power key -- so ids come out 0=A, 1=B, 2=PWR. */
#define BTN_PIN_A      GPIO_NUM_37
#define BTN_PIN_B      GPIO_NUM_39

/* Passive buzzer driven by an LEDC PWM channel. */
#define BUZZER_PIN     GPIO_NUM_2

/* Red LED on GPIO10, active-low, on its own LEDC timer/channel. */
#define LED_PIN        GPIO_NUM_10

/* 1S Li-ion endpoints for the coarse battery gauge. */
#define BATT_EMPTY_MV  3000
#define BATT_FULL_MV   4200

static axp192_handle_t s_axp;

static esp_err_t i2c_bus_init(i2c_master_bus_handle_t *out_bus) {
    const i2c_master_bus_config_t cfg = {
        .i2c_port          = I2C_PORT,
        .sda_io_num        = I2C_PIN_SDA,
        .scl_io_num        = I2C_PIN_SCL,
        .clk_source        = I2C_CLK_SRC_DEFAULT,
        .glitch_ignore_cnt = 7,
        .flags.enable_internal_pullup = true,
    };
    esp_err_t err = i2c_new_master_bus(&cfg, out_bus);
    if (err != ESP_OK) ESP_LOGE(TAG, "i2c_new_master_bus: %s", esp_err_to_name(err));
    return err;
}

/* Backlight = AXP192 LDO2 voltage, 2.5-3.3 V mapped over 0..100; 0 cuts LDO2. */
static void backlight_set(void *ctx, int brightness) {
    axp192_handle_t axp = ctx;
    if (brightness <= 0) {
        axp192_set_rail_enabled(axp, AXP192_RAIL_LDO2, false);
        return;
    }
    if (brightness > 100) brightness = 100;
    axp192_set_ldo2_mv(axp, 2500 + (3300 - 2500) * brightness / 100);
    axp192_set_rail_enabled(axp, AXP192_RAIL_LDO2, true);
}

/* Panel IC rail = AXP192 LDO3; cutting it is the display's POWER_OFF (GRAM lost). */
static void panel_power_set(void *ctx, bool on) {
    axp192_set_rail_enabled(ctx, AXP192_RAIL_LDO3, on);
}

static esp_err_t power_init(i2c_master_bus_handle_t bus) {
    const axp192_config_t cfg = {
        .i2c_bus     = bus,
        .i2c_address = AXP192_I2C_ADDR,
        .clock_hz    = AXP192_I2C_DEFAULT_HZ,
    };
    esp_err_t err = axp192_create(&cfg, &s_axp);
    if (err != ESP_OK) return err;

    /* TFT IC on LDO3 (3.0 V), backlight on LDO2 (starts mid-brightness). */
    axp192_set_ldo3_mv(s_axp, 3000);
    axp192_set_rail_enabled(s_axp, AXP192_RAIL_LDO3, true);
    backlight_set(s_axp, 80);

    /* EXTEN gates the buzzer's supply rail. */
    axp192_set_rail_enabled(s_axp, AXP192_RAIL_EXTEN, true);
    vTaskDelay(pdMS_TO_TICKS(20));

    bsp_power_t *power = NULL;
    if (axp192_power_create(s_axp, BATT_EMPTY_MV, BATT_FULL_MV, &power) == ESP_OK) {
        bsp_power_set_active(power);
    }
    return ESP_OK;
}

/* BM8563 INT drives the AXP192 power-on latch (not a readable GPIO), so leave
 * int_io NC: the countdown timer's INT still asserts to reboot from power-off. */
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

static esp_err_t display_init(void) {
    const spi_bus_config_t bus_cfg = {
        .mosi_io_num     = LCD_PIN_MOSI,
        .miso_io_num     = -1,
        .sclk_io_num     = LCD_PIN_SCLK,
        .quadwp_io_num   = -1,
        .quadhd_io_num   = -1,
        .max_transfer_sz = ST7789V2_DMA_CHUNK_BYTES,
    };
    esp_err_t err = spi_bus_initialize(LCD_SPI_HOST, &bus_cfg, SPI_DMA_CH_AUTO);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "spi_bus_initialize: %s", esp_err_to_name(err));
        return err;
    }

    const st7789v2_config_t cfg = {
        .spi_host      = LCD_SPI_HOST,
        .cs_io         = LCD_PIN_CS,
        .dc_io         = LCD_PIN_DC,
        .reset_io      = LCD_PIN_RST,
        .clock_hz      = ST7789V2_SPI_DEFAULT_HZ,
        .width         = LCD_WIDTH,
        .height        = LCD_HEIGHT,
        .x_offset      = LCD_X_OFFSET,
        .y_offset      = LCD_Y_OFFSET,
        .madctl        = 0x00,
        .invert        = true,          /* IPS panel */
        .set_backlight = backlight_set,
        .backlight_ctx = s_axp,
        .set_panel_power = panel_power_set,
        .panel_power_ctx = s_axp,
    };
    bsp_display_t *display = NULL;
    err = st7789v2_create(&cfg, &display);
    if (err != ESP_OK) return err;
    bsp_display_set_active(display);
    return ESP_OK;
}

/* A/B as a level provider (interrupt-driven), the AXP192 power key as a polled
 * tick provider. Two independent providers, appended in id order. */
static void buttons_init(void) {
    static const gpio_button_pin_t pins[] = {
        { BTN_PIN_A, true },
        { BTN_PIN_B, true },
    };
    const gpio_button_config_t gcfg = { .pins = pins, .count = 2, .enable_pull = false };
    bsp_button_raw_t *gpio = NULL;
    if (gpio_button_create(&gcfg, &gpio) == ESP_OK) bsp_button_add_raw(gpio);
    else ESP_LOGW(TAG, "gpio buttons unavailable");

    bsp_button_t *pwr = NULL;
    if (axp192_button_create(s_axp, &pwr) == ESP_OK) bsp_button_add(pwr);
    else ESP_LOGW(TAG, "power key unavailable");
}

static esp_err_t audio_init(const bsp_config_t *config) {
    const pwm_buzzer_config_t cfg = {
        .pwm_io  = BUZZER_PIN,
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

static void led_init(void) {
    const pwm_led_config_t cfg = {
        .gpio       = LED_PIN,
        .timer      = LEDC_TIMER_1,
        .channel    = LEDC_CHANNEL_1,
        .active_low = true,
    };
    bsp_led_t *led = NULL;
    if (pwm_led_create(&cfg, &led) == ESP_OK) bsp_led_set_active(led);
    else ESP_LOGW(TAG, "led unavailable");
}

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_dispatch_configure(config ? config->dispatch.task_priority : 0,
                           config ? config->dispatch.task_affinity : -1);

    i2c_master_bus_handle_t i2c_bus = NULL;
    esp_err_t err = i2c_bus_init(&i2c_bus);
    if (err != ESP_OK) return err;

    err = power_init(i2c_bus);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "power_init: %s", esp_err_to_name(err));
        return err;
    }

    if ((err = rtc_init(i2c_bus)) != ESP_OK) {
        ESP_LOGW(TAG, "rtc unavailable: %s", esp_err_to_name(err));
    }

    err = display_init();
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "display_init: %s", esp_err_to_name(err));
        return err;
    }

    buttons_init();
    led_init();

    if (config && (err = audio_init(config)) != ESP_OK) {
        ESP_LOGW(TAG, "audio unavailable: %s", esp_err_to_name(err));
    }
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
    if (s_axp) axp192_power_off(s_axp);
    return ESP_FAIL;   /* returning means VBUS held the rail up */
}
