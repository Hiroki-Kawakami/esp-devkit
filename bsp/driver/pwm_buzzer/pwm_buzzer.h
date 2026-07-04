/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Passive buzzer on an LEDC PWM channel — a TONE-only bsp_audio provider.
 * Continuous PWM at the tone frequency with fixed duty; the pin idles low
 * when silent, so an AC-coupled gate drive works.
 */

#pragma once
#include "bsp_types.h"
#include "bsp_audio.h"
#include "driver/gpio.h"
#include "driver/ledc.h"

typedef struct {
    gpio_num_t     pwm_io;
    ledc_timer_t   timer;    /*!< LEDC timer to own (reconfigured per tone) */
    ledc_channel_t channel;  /*!< LEDC channel to own */
} pwm_buzzer_config_t;

esp_err_t pwm_buzzer_create(const pwm_buzzer_config_t *config, bsp_audio_t **out_audio);
