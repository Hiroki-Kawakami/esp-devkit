/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Monochrome LED on an LEDC PWM channel — a brightness-only bsp_led provider
 * (count == 1). set_brightness maps 0..255 to duty; set_rgb is unsupported.
 * active_low inverts the duty so the pin idles in the LED-off state.
 */

#pragma once
#include "bsp_types.h"
#include "bsp_led.h"
#include "driver/gpio.h"
#include "driver/ledc.h"

typedef struct {
    gpio_num_t     gpio;
    ledc_timer_t   timer;      /*!< LEDC timer to own */
    ledc_channel_t channel;    /*!< LEDC channel to own */
    bool           active_low; /*!< pin low = LED on */
    uint32_t       freq_hz;    /*!< PWM frequency; 0 -> 5 kHz */
} pwm_led_config_t;

esp_err_t pwm_led_create(const pwm_led_config_t *config, bsp_led_t **out_led);
