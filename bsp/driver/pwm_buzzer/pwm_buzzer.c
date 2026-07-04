/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Passive-buzzer bsp_audio provider -- see pwm_buzzer.h. The timer is
 * reconfigured to the requested frequency on every tone() (duty stays fixed
 * at 50%); tone_stop / an expiring duration timer drop the duty to 0, which
 * holds the pin low.
 */

#include "pwm_buzzer.h"
#include <stdlib.h>
#include "esp_check.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"

static const char *TAG = "pwm_buzzer";

#define PWM_BUZZER_SPEED_MODE  LEDC_LOW_SPEED_MODE
#define PWM_BUZZER_DUTY_RES    LEDC_TIMER_10_BIT
#define PWM_BUZZER_DUTY_ON     (1u << (LEDC_TIMER_10_BIT - 1))  /* 50% of 1024 */
#define PWM_BUZZER_MIN_HZ      20
#define PWM_BUZZER_MAX_HZ      20000
#define PWM_BUZZER_IDLE_HZ     1000  /* placeholder timer config before the first tone */

typedef struct {
    bsp_audio_t          base;
    pwm_buzzer_config_t  cfg;
    SemaphoreHandle_t     lock;
    esp_timer_handle_t    timer;  /* duration one-shot */
} pwm_buzzer_dev_t;

static inline void dev_lock(pwm_buzzer_dev_t *dev)   { xSemaphoreTake(dev->lock, portMAX_DELAY); }
static inline void dev_unlock(pwm_buzzer_dev_t *dev) { xSemaphoreGive(dev->lock); }

/* Duty 0 keeps the output GPIO continuously low -- the idle state required by
 * the board's AC-coupled gate drive. */
static esp_err_t silence(pwm_buzzer_dev_t *dev) {
    esp_err_t err = ledc_set_duty(PWM_BUZZER_SPEED_MODE, dev->cfg.channel, 0);
    if (err == ESP_OK) err = ledc_update_duty(PWM_BUZZER_SPEED_MODE, dev->cfg.channel);
    return err;
}

static void duration_cb(void *arg) {
    pwm_buzzer_dev_t *dev = arg;
    dev_lock(dev);
    silence(dev);
    dev_unlock(dev);
}

static esp_err_t pb_tone(bsp_audio_t *self, uint32_t freq_hz, uint32_t duration_ms) {
    pwm_buzzer_dev_t *dev = (pwm_buzzer_dev_t *)self;
    if (freq_hz < PWM_BUZZER_MIN_HZ || freq_hz > PWM_BUZZER_MAX_HZ) return ESP_ERR_INVALID_ARG;

    dev_lock(dev);
    esp_timer_stop(dev->timer);  /* ESP_ERR_INVALID_STATE if not running -- fine */

    const ledc_timer_config_t timer_cfg = {
        .speed_mode      = PWM_BUZZER_SPEED_MODE,
        .duty_resolution = PWM_BUZZER_DUTY_RES,
        .timer_num       = dev->cfg.timer,
        .freq_hz         = freq_hz,
        .clk_cfg         = LEDC_AUTO_CLK,
    };
    esp_err_t err = ledc_timer_config(&timer_cfg);
    if (err != ESP_OK) { dev_unlock(dev); return ESP_ERR_INVALID_ARG; }

    err = ledc_set_duty(PWM_BUZZER_SPEED_MODE, dev->cfg.channel, PWM_BUZZER_DUTY_ON);
    if (err == ESP_OK) err = ledc_update_duty(PWM_BUZZER_SPEED_MODE, dev->cfg.channel);
    if (err == ESP_OK && duration_ms) {
        err = esp_timer_start_once(dev->timer, (uint64_t)duration_ms * 1000);
    }
    dev_unlock(dev);
    return err;
}

static esp_err_t pb_tone_stop(bsp_audio_t *self) {
    pwm_buzzer_dev_t *dev = (pwm_buzzer_dev_t *)self;
    dev_lock(dev);
    esp_timer_stop(dev->timer);
    esp_err_t err = silence(dev);
    dev_unlock(dev);
    return err;
}

static esp_err_t pb_deinit(bsp_audio_t *self) {
    pwm_buzzer_dev_t *dev = (pwm_buzzer_dev_t *)self;
    if (!dev) return ESP_OK;
    if (dev->timer) {
        esp_timer_stop(dev->timer);
        esp_timer_delete(dev->timer);
    }
    silence(dev);
    if (dev->lock) vSemaphoreDelete(dev->lock);
    free(dev);
    return ESP_OK;
}

esp_err_t pwm_buzzer_create(const pwm_buzzer_config_t *config, bsp_audio_t **out_audio) {
    ESP_RETURN_ON_FALSE(config && out_audio, ESP_ERR_INVALID_ARG, TAG, "null arg");

    pwm_buzzer_dev_t *dev = calloc(1, sizeof(*dev));
    if (!dev) return ESP_ERR_NO_MEM;
    dev->cfg = *config;
    dev->base.caps      = BSP_AUDIO_CAP_TONE;
    dev->base.tone      = pb_tone;
    dev->base.tone_stop = pb_tone_stop;
    dev->base.deinit    = pb_deinit;

    dev->lock = xSemaphoreCreateMutex();
    if (!dev->lock) { free(dev); return ESP_ERR_NO_MEM; }

    const ledc_timer_config_t timer_cfg = {
        .speed_mode      = PWM_BUZZER_SPEED_MODE,
        .duty_resolution = PWM_BUZZER_DUTY_RES,
        .timer_num       = config->timer,
        .freq_hz         = PWM_BUZZER_IDLE_HZ,
        .clk_cfg         = LEDC_AUTO_CLK,
    };
    esp_err_t err = ledc_timer_config(&timer_cfg);
    if (err != ESP_OK) { pb_deinit(&dev->base); return err; }

    const ledc_channel_config_t ch_cfg = {
        .speed_mode = PWM_BUZZER_SPEED_MODE,
        .channel    = config->channel,
        .timer_sel  = config->timer,
        .gpio_num   = config->pwm_io,
        .duty       = 0,
        .hpoint     = 0,
    };
    err = ledc_channel_config(&ch_cfg);
    if (err != ESP_OK) { pb_deinit(&dev->base); return err; }

    const esp_timer_create_args_t timer_args = {
        .callback = duration_cb,
        .arg      = dev,
        .name     = "pwm_buzzer",
    };
    err = esp_timer_create(&timer_args, &dev->timer);
    if (err != ESP_OK) { pb_deinit(&dev->base); return err; }

    *out_audio = &dev->base;
    return ESP_OK;
}
