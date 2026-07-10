/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack CoreS3 (ESP32-S3) board entry: brings up the shared I2C bus, the
 * AXP2101 PMIC rails and the AW9523B I/O expander, then hands off to
 * core_s3_panel_init for the ILI9342C display (touch comes later). The whole
 * CoreS3 family shares this wiring. Host-side counterpart: core_s3_sim.c.
 */

#include "bsp.h"
#include "esp_system.h"
#include "esp_log.h"
#include "driver/i2c_master.h"
#include "axp2101.h"
#include "aw9523.h"
#include "core_s3_panel.h"
#include "bsp_power.h"
#include "bsp_dispatch.h"

static const char *TAG = "core_s3";

#define I2C_PORT       I2C_NUM_0
#define I2C_PIN_SDA    GPIO_NUM_12
#define I2C_PIN_SCL    GPIO_NUM_11

#define AXP2101_ADDR   0x34
#define AW9523B_ADDR   0x58

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

    /* The LCD logic rail sits somewhere in the LDO tree M5GFX turns on wholesale
     * (reg0x90 = 0xBF); enable the same set so the panel has power. */
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

/* AW9523B outputs high on P0_0, P0_2, P1_0, P1_1 (touch/amp reset release, LCD
 * power/bus enable, LCD reset release); the rest of the driven set low; P0_3/P0_4/
 * P1_2/P1_3 inputs -- same registers M5GFX writes. P1_0 gates the LCD power. */
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
    return aw9523_init(bus, AW9523B_ADDR, pins, &s_aw);
}

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_dispatch_configure(config ? config->dispatch.task_priority : 0,
                           config ? config->dispatch.task_affinity : -1);

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
    return ESP_OK;
}

void bsp_power_restart(void) {
    esp_restart();
}

esp_err_t bsp_power_off(void) {
    if (s_axp) axp2101_power_off(s_axp);
    return ESP_FAIL;   /* returning means VBUS held the rail up */
}
