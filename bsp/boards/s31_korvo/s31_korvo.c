/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ESP32-S31-Korvo board (ESP32-S31 + 800x480 RGB parallel LCD). Minimum
 * bring-up: hands off to s31_korvo_panel_init for the RGB LCD. I2C bus (SDA=0,
 * SCL=1) and GT1151 touch will be added when touch support lands.
 */

#include "bsp.h"
#include "bsp_led.h"
#include "esp_system.h"
#include "esp_log.h"
#include "driver/i2c_master.h"
#include "s31_korvo_panel.h"
#include "ws2812.h"

static const char *TAG = "s31_korvo";

#define S31_I2C_PORT     I2C_NUM_0
#define S31_I2C_PIN_SDA  GPIO_NUM_0
#define S31_I2C_PIN_SCL  GPIO_NUM_1

#define S31_LED_GPIO     GPIO_NUM_37
#define S31_LED_COUNT    1

static esp_err_t i2c_bus_init(i2c_master_bus_handle_t *out_bus) {
    const i2c_master_bus_config_t cfg = {
        .i2c_port          = S31_I2C_PORT,
        .sda_io_num        = S31_I2C_PIN_SDA,
        .scl_io_num        = S31_I2C_PIN_SCL,
        .clk_source        = I2C_CLK_SRC_DEFAULT,
        .flags.enable_internal_pullup = true,
    };
    esp_err_t err = i2c_new_master_bus(&cfg, out_bus);
    if (err != ESP_OK) ESP_LOGE(TAG, "i2c_new_master_bus: %s", esp_err_to_name(err));
    return err;
}

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_config_t defaults = {0};
    if (!config) config = &defaults;

    i2c_master_bus_handle_t i2c_bus = NULL;
    esp_err_t err = i2c_bus_init(&i2c_bus);
    if (err != ESP_OK) return err;

    /* WS2812 status LED is optional -- log and continue on failure so a bad
     * RMT bring-up doesn't take the display + touch path down with it. */
    const ws2812_config_t led_cfg = {
        .gpio  = S31_LED_GPIO,
        .count = S31_LED_COUNT,
    };
    bsp_led_t *led = NULL;
    if (ws2812_create(&led_cfg, &led) == ESP_OK) {
        bsp_led_set_active(led);
    } else {
        ESP_LOGW(TAG, "ws2812 unavailable");
    }

    return s31_korvo_panel_init(config, i2c_bus);
}

void bsp_restart(void) {
    esp_restart();
}

esp_err_t bsp_hw_reset(void) {
    esp_restart();
    return ESP_FAIL;
}

esp_err_t bsp_power_off(void) {
    return ESP_ERR_NOT_SUPPORTED;
}
