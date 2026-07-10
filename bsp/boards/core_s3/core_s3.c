/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack CoreS3 (ESP32-S3) board entry: brings up the shared I2C bus, the
 * AXP2101 PMIC rails and the AW9523B I/O expander, then hands off to
 * core_s3_panel_init for the ILI9342C display + FT6336U touch. The FT6336U INT is
 * aggregated on AW9523B P1_2 whose expander INT output lands on GPIO21, so this
 * file owns that ISR: it clears the expander latch (task context) and wakes the
 * touch layer. The whole CoreS3 family shares this wiring. Host-side counterpart:
 * core_s3_sim.c.
 */

#include "bsp.h"
#include "esp_system.h"
#include "esp_log.h"
#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "freertos/FreeRTOS.h"
#include "axp2101.h"
#include "aw9523.h"
#include "core_s3_panel.h"
#include "core_s3_audio.h"
#include "bsp_power.h"
#include "bsp_audio.h"
#include "bsp_touch.h"
#include "bsp_dispatch.h"

static const char *TAG = "core_s3";

#define I2C_PORT       I2C_NUM_0
#define I2C_PIN_SDA    GPIO_NUM_12
#define I2C_PIN_SCL    GPIO_NUM_11

#define AXP2101_ADDR   0x34
#define AW9523B_ADDR   0x58

/* AW9523B aggregated INT output (FT6336U touch INT is on its P1_2 input). */
#define AW9523_INT_IO  GPIO_NUM_21
#define AW_PIN_TOUCH_INT 10   /* AW9523B P1_2 -> 8 + 2 */

/* 1S Li-ion endpoints for the coarse battery gauge. */
#define BATT_EMPTY_MV  3300
#define BATT_FULL_MV   4200

static axp2101_handle_t s_axp;
static aw9523_t         s_aw;

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

static esp_err_t power_init(i2c_master_bus_handle_t bus) {
    const axp2101_config_t cfg = {
        .i2c_bus     = bus,
        .i2c_address = AXP2101_ADDR,
        .clock_hz    = AXP2101_I2C_DEFAULT_HZ,
    };
    esp_err_t err = axp2101_create(&cfg, &s_axp);
    if (err != ESP_OK) return err;

    axp2101_set_rail_mv(s_axp, AXP2101_ALDO3, 3300);
    axp2101_set_rail_mv(s_axp, AXP2101_ALDO4, 3300);
    axp2101_set_rail_enabled(s_axp, AXP2101_ALDO1, true);
    axp2101_set_rail_enabled(s_axp, AXP2101_ALDO2, true);
    axp2101_set_rail_enabled(s_axp, AXP2101_ALDO3, true);
    axp2101_set_rail_enabled(s_axp, AXP2101_ALDO4, true);
    axp2101_set_rail_enabled(s_axp, AXP2101_BLDO1, true);
    axp2101_set_rail_enabled(s_axp, AXP2101_BLDO2, true);

    bsp_power_t *power = NULL;
    if (axp2101_power_create(s_axp, BATT_EMPTY_MV, BATT_FULL_MV, &power) == ESP_OK)
        bsp_power_set_active(power);
    return ESP_OK;
}

/* AW9523B pin setup: P0_0/P1_0/P1_1 high (resets + LCD power/bus
 * enable), P0_2 high (AW88298 power -- must be high or the codec won't
 * answer on I2C), P1_2 input with change-INT = FT6336U touch INT. */
static esp_err_t expander_init(i2c_master_bus_handle_t bus) {
    static const uint8_t high[] = { 0, 2, 8, 9 };
    static const uint8_t low[]  = { 1, 5, 6, 7, 12, 13, 14, 15 };

    aw9523_pin_config_t pins[16] = {0};
    for (unsigned i = 0; i < sizeof(high) / sizeof(high[0]); i++)
        pins[high[i]] = (aw9523_pin_config_t){ .mode = AW9523_PIN_MODE_OUTPUT,
                                               .initial_value = true };
    for (unsigned i = 0; i < sizeof(low) / sizeof(low[0]); i++)
        pins[low[i]] = (aw9523_pin_config_t){ .mode = AW9523_PIN_MODE_OUTPUT,
                                              .initial_value = false };
    pins[AW_PIN_TOUCH_INT] = (aw9523_pin_config_t){ .mode = AW9523_PIN_MODE_INPUT,
                                                    .interrupt = true };
    return aw9523_init(bus, AW9523B_ADDR, pins, &s_aw);
}

/* Expander INT (GPIO21) service: the ISR only wakes this source; the tick runs in
 * task context (I2C-capable) to clear the AW9523B latch and wake the touch layer. */
static uint32_t expander_int_tick(void *ctx) {
    (void)ctx;
    aw9523_read_inputs(s_aw, NULL);
    bsp_touch_notify();
    return BSP_DISPATCH_IDLE;
}
static bsp_dispatch_source_t s_expander_src = { .tick = expander_int_tick };

static void IRAM_ATTR expander_int_isr(void *arg) {
    (void)arg;
    BaseType_t hp = pdFALSE;
    bsp_dispatch_notify_from_isr(&s_expander_src, &hp);
    if (hp) portYIELD_FROM_ISR();
}

static esp_err_t expander_int_init(void) {
    const gpio_config_t cfg = {
        .pin_bit_mask = 1ULL << AW9523_INT_IO,
        .mode         = GPIO_MODE_INPUT,
        .pull_up_en   = GPIO_PULLUP_ENABLE,
    };
    esp_err_t err = gpio_config(&cfg);
    if (err != ESP_OK) return err;
    if ((err = bsp_dispatch_install_gpio_isr()) != ESP_OK) return err;
    gpio_set_intr_type(AW9523_INT_IO, GPIO_INTR_NEGEDGE);
    if ((err = gpio_isr_handler_add(AW9523_INT_IO, expander_int_isr, NULL)) != ESP_OK) return err;
    gpio_intr_enable(AW9523_INT_IO);
    bsp_dispatch_add_source(&s_expander_src);
    return ESP_OK;
}

/* AW88298 speaker over I2S1 + its AW9523B P0_2 amp gate. Non-fatal: an amp
 * failure leaves the bsp_audio_* API a no-op rather than blocking bring-up. */
static esp_err_t audio_init(const bsp_config_t *config, i2c_master_bus_handle_t bus) {
    bsp_audio_t *audio = NULL;
    esp_err_t err = core_s3_audio_create(&(core_s3_audio_config_t){
        .i2c_bus     = bus,
        .io_expander = s_aw,
    }, &audio);
    if (err != ESP_OK) return err;
    bsp_audio_set_active(audio, &(bsp_audio_init_t){
        .dsp_mode     = config->audio.dsp_mode,
        .speaker_mode = config->audio.speaker_mode,
    });
    return ESP_OK;
}

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_dispatch_configure(config ? config->dispatch.task_priority : 0,
                           config ? config->dispatch.task_affinity : -1);

    bsp_config_t defaults = {0};
    if (!config) config = &defaults;

    i2c_master_bus_handle_t i2c_bus = NULL;
    esp_err_t err = i2c_bus_init(&i2c_bus);
    if (err != ESP_OK) return err;

    if ((err = power_init(i2c_bus)) != ESP_OK) {
        ESP_LOGE(TAG, "power_init: %s", esp_err_to_name(err));
        return err;
    }
    if ((err = expander_init(i2c_bus)) != ESP_OK) {
        ESP_LOGE(TAG, "expander_init: %s", esp_err_to_name(err));
        return err;
    }
    if ((err = core_s3_panel_init(i2c_bus, s_axp, s_aw)) != ESP_OK) {
        ESP_LOGE(TAG, "panel_init: %s", esp_err_to_name(err));
        return err;
    }
    if ((err = audio_init(config, i2c_bus)) != ESP_OK)
        ESP_LOGW(TAG, "audio unavailable: %s", esp_err_to_name(err));
    if ((err = expander_int_init()) != ESP_OK)
        ESP_LOGW(TAG, "expander INT unavailable: %s", esp_err_to_name(err));
    return ESP_OK;
}

void bsp_power_restart(void) {
    bsp_audio_quiesce();
    esp_restart();
}

esp_err_t bsp_power_off(void) {
    bsp_audio_quiesce();
    if (s_axp) axp2101_power_off(s_axp);
    return ESP_FAIL;   /* returning means VBUS held the rail up */
}
