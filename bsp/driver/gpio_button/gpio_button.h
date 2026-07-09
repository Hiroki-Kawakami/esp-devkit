/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Plain GPIO momentary buttons: one pin per button, sampled by level. Returns a
 * bsp_button_t vtable that the common layer (src/bsp_button.c) drives -- register
 * it with bsp_button_add_raw(). For ADC-ladder buttons use adc_button instead.
 */

#pragma once
#include "bsp_button.h"
#include "driver/gpio.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    gpio_num_t gpio;
    bool       active_low;   /* pressed == level low */
} gpio_button_pin_t;

typedef struct {
    const gpio_button_pin_t *pins;
    uint8_t                  count;
    bool                     enable_pull;   /* internal pull toward the released level */
} gpio_button_config_t;

esp_err_t gpio_button_create(const gpio_button_config_t *cfg, bsp_button_raw_t **out_button);

#ifdef __cplusplus
}
#endif
