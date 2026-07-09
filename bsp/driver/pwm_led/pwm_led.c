/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Monochrome PWM LED bsp_led provider — see pwm_led.h.
 */

#include "pwm_led.h"
#include <stdlib.h>
#include "esp_check.h"

static const char *TAG = "pwm_led";

#define PWM_LED_SPEED_MODE  LEDC_LOW_SPEED_MODE
#define PWM_LED_DUTY_RES    LEDC_TIMER_10_BIT
#define PWM_LED_DUTY_MAX    ((1u << LEDC_TIMER_10_BIT) - 1)
#define PWM_LED_DEFAULT_HZ  5000

typedef struct {
    bsp_led_t       base;
    pwm_led_config_t cfg;
} pwm_led_dev_t;

static esp_err_t pwm_led_apply(pwm_led_dev_t *dev, uint8_t level) {
    uint32_t duty = (uint32_t)level * PWM_LED_DUTY_MAX / 255;
    if (dev->cfg.active_low) duty = PWM_LED_DUTY_MAX - duty;
    esp_err_t err = ledc_set_duty(PWM_LED_SPEED_MODE, dev->cfg.channel, duty);
    if (err == ESP_OK) err = ledc_update_duty(PWM_LED_SPEED_MODE, dev->cfg.channel);
    return err;
}

static esp_err_t pwm_led_set_brightness(bsp_led_t *self, uint8_t index, uint8_t level) {
    (void)index;
    return pwm_led_apply((pwm_led_dev_t *)self, level);
}

static esp_err_t pwm_led_clear(bsp_led_t *self) {
    return pwm_led_apply((pwm_led_dev_t *)self, 0);
}

static esp_err_t pwm_led_deinit(bsp_led_t *self) {
    pwm_led_dev_t *dev = (pwm_led_dev_t *)self;
    if (!dev) return ESP_OK;
    pwm_led_apply(dev, 0);
    ledc_stop(PWM_LED_SPEED_MODE, dev->cfg.channel, dev->cfg.active_low ? 1 : 0);
    free(dev);
    return ESP_OK;
}

esp_err_t pwm_led_create(const pwm_led_config_t *config, bsp_led_t **out_led) {
    ESP_RETURN_ON_FALSE(config && out_led, ESP_ERR_INVALID_ARG, TAG, "null arg");
    ESP_RETURN_ON_FALSE(config->gpio != GPIO_NUM_NC, ESP_ERR_INVALID_ARG, TAG, "gpio required");

    pwm_led_dev_t *dev = calloc(1, sizeof(*dev));
    if (!dev) return ESP_ERR_NO_MEM;
    dev->cfg = *config;
    dev->base.count          = 1;
    dev->base.set_brightness = pwm_led_set_brightness;
    dev->base.clear          = pwm_led_clear;
    dev->base.deinit         = pwm_led_deinit;

    const ledc_timer_config_t timer_cfg = {
        .speed_mode      = PWM_LED_SPEED_MODE,
        .duty_resolution = PWM_LED_DUTY_RES,
        .timer_num       = config->timer,
        .freq_hz         = config->freq_hz ? config->freq_hz : PWM_LED_DEFAULT_HZ,
        .clk_cfg         = LEDC_AUTO_CLK,
    };
    esp_err_t err = ledc_timer_config(&timer_cfg);
    if (err != ESP_OK) { free(dev); return err; }

    const ledc_channel_config_t ch_cfg = {
        .speed_mode = PWM_LED_SPEED_MODE,
        .channel    = config->channel,
        .timer_sel  = config->timer,
        .gpio_num   = config->gpio,
        .duty       = config->active_low ? PWM_LED_DUTY_MAX : 0,  /* start off */
        .hpoint     = 0,
    };
    err = ledc_channel_config(&ch_cfg);
    if (err != ESP_OK) { free(dev); return err; }

    *out_led = &dev->base;
    return ESP_OK;
}
