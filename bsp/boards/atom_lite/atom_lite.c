/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack ATOM Lite (ESP32). No display: bring-up wires only the single
 * WS2812 RGB LED (GPIO27) and the front button (GPIO39). The button pin is
 * input-only with an external pull-up on the board, so no internal pull is
 * requested.
 */

#include "bsp.h"
#include "bsp_led.h"
#include "bsp_button.h"
#include "bsp_dispatch.h"
#include "esp_log.h"
#include "ws2812.h"
#include "gpio_button.h"

static const char *TAG = "atom_lite";

#define ATOM_LED_GPIO      GPIO_NUM_27
#define ATOM_LED_COUNT     1
#define ATOM_BUTTON_GPIO   GPIO_NUM_39

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_dispatch_configure(config ? config->dispatch.task_priority : 0,
                           config ? config->dispatch.task_affinity : -1);

    const ws2812_config_t led_cfg = {
        .gpio  = ATOM_LED_GPIO,
        .count = ATOM_LED_COUNT,
    };
    bsp_led_t *led = NULL;
    if (ws2812_create(&led_cfg, &led) == ESP_OK) {
        bsp_led_set_active(led);
    } else {
        ESP_LOGW(TAG, "ws2812 unavailable");
    }

    static const gpio_button_pin_t btn_pins[] = {
        { .gpio = ATOM_BUTTON_GPIO, .active_low = true },
    };
    const gpio_button_config_t btn_cfg = {
        .pins        = btn_pins,
        .count       = 1,
        .enable_pull = false,
    };
    bsp_button_raw_t *btn = NULL;
    if (gpio_button_create(&btn_cfg, &btn) == ESP_OK) {
        bsp_button_add_raw(btn);
    } else {
        ESP_LOGW(TAG, "gpio_button unavailable");
    }

    return ESP_OK;
}
