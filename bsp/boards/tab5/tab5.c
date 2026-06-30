/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Tab5 board (ESP32-P4 + ILI9881C MIPI-DSI LCD + GT911 touch). Brings up
 * the I2C bus shared by the touch chip and the two PI4IOE5V6408 I/O expanders,
 * runs the GT911 INT-pulse reset sequence required across LCD/TP_RST (both
 * routed through PI4IOE1), then hands off to tab5_panel_init for display +
 * touch. Audio, USB host, SD, and RTC are not wired here — add them when needed.
 */

#include "bsp.h"
#include "esp_system.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "pi4io.h"
#include "tab5_panel.h"

static const char *TAG = "tab5";

#define TAB5_I2C_PORT     I2C_NUM_0
#define TAB5_I2C_PIN_SDA  GPIO_NUM_31
#define TAB5_I2C_PIN_SCL  GPIO_NUM_32

#define TAB5_PI4IOE1_ADDR 0x43
#define TAB5_PI4IOE2_ADDR 0x44

#define TAB5_TOUCH_PIN_INT GPIO_NUM_23

static pi4io_t s_pi4ioe1, s_pi4ioe2;

static esp_err_t i2c_bus_init(i2c_master_bus_handle_t *out_bus) {
    const i2c_master_bus_config_t cfg = {
        .i2c_port          = TAB5_I2C_PORT,
        .sda_io_num        = TAB5_I2C_PIN_SDA,
        .scl_io_num        = TAB5_I2C_PIN_SCL,
        .clk_source        = I2C_CLK_SRC_DEFAULT,
        .flags.enable_internal_pullup = true,
    };
    esp_err_t err = i2c_new_master_bus(&cfg, out_bus);
    if (err != ESP_OK) ESP_LOGE(TAG, "i2c_new_master_bus: %s", esp_err_to_name(err));
    return err;
}

/* PI4IOE1 (0x43) owns LCD_RST + TP_RST + SPK_EN + the headphone-detect input;
 * PI4IOE2 (0x44) owns the power rails (USB5V, PWROFF, charger). SPK_EN starts
 * LOW so the codec stays muted until an app's audio bring-up enables it. */
static esp_err_t io_expanders_init(i2c_master_bus_handle_t bus) {
    pi4io_pin_config_t pi4ioe1_cfg[8] = {
        [0] = { PI4IO_PIN_MODE_OUTPUT, .initial_value = false },  // RF_INT_EXT_SWITCH
        [1] = { PI4IO_PIN_MODE_OUTPUT, .initial_value = false },  // SPK_EN
        [2] = { PI4IO_PIN_MODE_OUTPUT, .initial_value = true  },  // EXT5V_EN
        [4] = { PI4IO_PIN_MODE_OUTPUT, .initial_value = true  },  // LCD_RST
        [5] = { PI4IO_PIN_MODE_OUTPUT, .initial_value = true  },  // TP_RST
        [6] = { PI4IO_PIN_MODE_OUTPUT, .initial_value = true  },  // CAM_RST
        [7] = { PI4IO_PIN_MODE_INPUT  },                          // HP_DET
    };
    esp_err_t err = pi4io_init(bus, TAB5_PI4IOE1_ADDR, pi4ioe1_cfg, &s_pi4ioe1);
    if (err != ESP_OK) return err;

    pi4io_pin_config_t pi4ioe2_cfg[8] = {
        [0] = { PI4IO_PIN_MODE_OUTPUT, .initial_value = true  },  // WLAN_PWR_EN
        [3] = { PI4IO_PIN_MODE_OUTPUT, .initial_value = false },  // USB5V_EN
        [4] = { PI4IO_PIN_MODE_OUTPUT, .initial_value = false },  // PWROFF_PLUSE
        [5] = { PI4IO_PIN_MODE_OUTPUT, .initial_value = false },  // nCHG_QC_EN
        [6] = { PI4IO_PIN_MODE_INPUT },                           // CHG_STAT
        [7] = { PI4IO_PIN_MODE_OUTPUT, .initial_value = false },  // CHG_EN
    };
    return pi4io_init(bus, TAB5_PI4IOE2_ADDR, pi4ioe2_cfg, &s_pi4ioe2);
}

/* The GT911 needs the full AOSP gtp_reset_guitar sequence to boot its touch
 * firmware: hold INT high across TP_RST's rising edge (selects 0x14), then
 * drive INT low for 100 ms and release. A cold POR hides the omission, but
 * esp_restart leaves the touch rail powered, so this is required for touch to
 * survive a warm reboot. TP_RST + LCD_RST are PI4IOE1 pins, not native GPIOs. */
static void reset_panel_and_touch(void) {
    gpio_set_direction(TAB5_TOUCH_PIN_INT, GPIO_MODE_OUTPUT);
    gpio_set_level(TAB5_TOUCH_PIN_INT, 1);          // latch GT911 addr 0x14
    pi4io_set_output(s_pi4ioe1, 4, false);          // LCD_RST low
    pi4io_set_output(s_pi4ioe1, 5, false);          // TP_RST  low
    vTaskDelay(pdMS_TO_TICKS(100));
    pi4io_set_output(s_pi4ioe1, 4, true);           // LCD_RST high
    pi4io_set_output(s_pi4ioe1, 5, true);           // TP_RST  high (rising edge latches INT)
    vTaskDelay(pdMS_TO_TICKS(20));
    gpio_set_level(TAB5_TOUCH_PIN_INT, 0);          // INT low boots the touch firmware ...
    vTaskDelay(pdMS_TO_TICKS(100));                 // ... held 100 ms
    gpio_set_direction(TAB5_TOUCH_PIN_INT, GPIO_MODE_INPUT);
    vTaskDelay(pdMS_TO_TICKS(50));
}

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_config_t defaults = {0};
    if (!config) config = &defaults;

    i2c_master_bus_handle_t i2c_bus = NULL;
    esp_err_t err = i2c_bus_init(&i2c_bus);
    if (err != ESP_OK) return err;

    err = io_expanders_init(i2c_bus);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "io_expanders_init: %s", esp_err_to_name(err));
        return err;
    }

    reset_panel_and_touch();

    return tab5_panel_init(config, i2c_bus);
}

void bsp_restart(void) {
    esp_restart();
}

esp_err_t bsp_hw_reset(void) {
    return bsp_power_off();
}

esp_err_t bsp_power_off(void) {
    if (!s_pi4ioe2) return ESP_ERR_INVALID_STATE;
    pi4io_set_output(s_pi4ioe2, 4, true);   // PWROFF_PLUSE
    vTaskDelay(pdMS_TO_TICKS(500));
    pi4io_set_output(s_pi4ioe2, 4, false);
    return ESP_FAIL;
}
