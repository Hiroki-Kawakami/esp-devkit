/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Core audio. Core2 drives an NS4168 over I2S (BCLK=G12, LRCK=G0,
 * DATA=G2) with the amp gate on AXP192 GPIO2; Basic plays through the ESP32
 * built-in DAC on GPIO25. Both wrap a generic driver as a mono-speaker
 * bsp_audio provider; volume/mute live in the driver's software gain.
 */

#include "core_audio.h"
#include <stdlib.h>
#include "i2s_dac.h"
#include "bsp_audio.h"

#define I2S_PIN_BCLK   GPIO_NUM_12
#define I2S_PIN_LRCK   GPIO_NUM_0
#define I2S_PIN_DATA   GPIO_NUM_2

#define AXP_REG_GPIO2_FUNC  0x93
#define AXP_REG_GPIO_LVL    0x94
#define AXP_GPIO2_FUNC_MASK 0x07
#define AXP_GPIO2_OD_OUT    0x00   /* NMOS open-drain output */
#define AXP_GPIO2_LVL_BIT   0x04

/* Small internal driver voicing (mono): high-pass out the sub, lift the
 * low-mid body, downmix to mono. Same for both output paths. */
static esp_err_t core_dsp_profile(bsp_audio_t *self, bool headphone, uint32_t sample_rate,
                                  bsp_audio_dsp_profile_t *out) {
    (void)self; (void)headphone;
    const uint32_t fs = sample_rate ? sample_rate : 48000;
    out->biquads[0] = audio_dsp_design_highpass (fs,  80.0f, 0.707f);
    out->biquads[1] = audio_dsp_design_low_shelf(fs, 300.0f, 0.707f, +7.0f);
    out->biquads[2] = audio_dsp_design_peaking  (fs, 150.0f, 1.20f,  +3.0f);
    out->num_stages = 3;
    out->mono_mix   = true;
    out->eq_enabled = true;
    return ESP_OK;
}

/* ---- Core2: NS4168 over I2S, amp gate on AXP192 GPIO2 ---- */

typedef struct {
    bsp_audio_t     base;
    i2s_dac_t       dac;
    axp192_handle_t axp;
} core2_audio_t;

static esp_err_t c2_open(bsp_audio_t *self, uint32_t r, uint8_t b, uint8_t c) {
    return i2s_dac_open(((core2_audio_t *)self)->dac, r, b, c);
}
static esp_err_t c2_close(bsp_audio_t *self) {
    return i2s_dac_close(((core2_audio_t *)self)->dac);
}
static esp_err_t c2_write(bsp_audio_t *self, const void *data, size_t len) {
    return i2s_dac_write(((core2_audio_t *)self)->dac, data, len);
}
static esp_err_t c2_set_volume(bsp_audio_t *self, int volume) {
    return i2s_dac_set_volume(((core2_audio_t *)self)->dac, volume);
}
static esp_err_t c2_set_mute(bsp_audio_t *self, bool mute) {
    return i2s_dac_set_mute(((core2_audio_t *)self)->dac, mute);
}
static esp_err_t c2_set_speaker(bsp_audio_t *self, bool enabled) {
    axp192_handle_t axp = ((core2_audio_t *)self)->axp;
    uint8_t lvl = 0;
    esp_err_t err = axp192_read_reg(axp, AXP_REG_GPIO_LVL, &lvl);
    if (err != ESP_OK) return err;
    if (enabled) lvl |= AXP_GPIO2_LVL_BIT;
    else         lvl &= ~AXP_GPIO2_LVL_BIT;
    return axp192_write_reg(axp, AXP_REG_GPIO_LVL, lvl);
}
static esp_err_t c2_deinit(bsp_audio_t *self) {
    core2_audio_t *s = (core2_audio_t *)self;
    if (s->dac) i2s_dac_deinit(s->dac);
    free(s);
    return ESP_OK;
}

static esp_err_t core2_audio_create(axp192_handle_t axp, bsp_audio_t **out) {
    core2_audio_t *s = calloc(1, sizeof(*s));
    if (!s) return ESP_ERR_NO_MEM;

    esp_err_t err = i2s_dac_create(&(i2s_dac_config_t){
        .i2s_port  = I2S_NUM_0,
        .bclk_gpio = I2S_PIN_BCLK,
        .ws_gpio   = I2S_PIN_LRCK,
        .dout_gpio = I2S_PIN_DATA,
    }, &s->dac);
    if (err != ESP_OK) { free(s); return err; }
    s->axp = axp;

    uint8_t func = 0;
    axp192_read_reg(axp, AXP_REG_GPIO2_FUNC, &func);
    axp192_write_reg(axp, AXP_REG_GPIO2_FUNC, (func & ~AXP_GPIO2_FUNC_MASK) | AXP_GPIO2_OD_OUT);

    s->base.caps = BSP_AUDIO_CAP_PCM | BSP_AUDIO_CAP_SPEAKER;
    s->base.open                = c2_open;
    s->base.close               = c2_close;
    s->base.write               = c2_write;
    s->base.set_hw_volume       = c2_set_volume;
    s->base.set_hw_mute         = c2_set_mute;
    s->base.set_speaker_enabled = c2_set_speaker;
    s->base.get_dsp_profile     = core_dsp_profile;
    s->base.deinit              = c2_deinit;

    *out = &s->base;
    return ESP_OK;
}

/* ---- Basic: ESP32 built-in DAC on GPIO25 (no amp gate) ---- */

typedef struct {
    bsp_audio_t base;
    i2s_dac_t   dac;
} basic_audio_t;

static esp_err_t b_open(bsp_audio_t *self, uint32_t r, uint8_t b, uint8_t c) {
    return i2s_dac_open(((basic_audio_t *)self)->dac, r, b, c);
}
static esp_err_t b_close(bsp_audio_t *self) {
    return i2s_dac_close(((basic_audio_t *)self)->dac);
}
static esp_err_t b_write(bsp_audio_t *self, const void *data, size_t len) {
    return i2s_dac_write(((basic_audio_t *)self)->dac, data, len);
}
static esp_err_t b_set_volume(bsp_audio_t *self, int volume) {
    return i2s_dac_set_volume(((basic_audio_t *)self)->dac, volume);
}
static esp_err_t b_set_mute(bsp_audio_t *self, bool mute) {
    return i2s_dac_set_mute(((basic_audio_t *)self)->dac, mute);
}
static esp_err_t b_deinit(bsp_audio_t *self) {
    basic_audio_t *s = (basic_audio_t *)self;
    if (s->dac) i2s_dac_deinit(s->dac);
    free(s);
    return ESP_OK;
}

static esp_err_t basic_audio_create(bsp_audio_t **out) {
    basic_audio_t *s = calloc(1, sizeof(*s));
    if (!s) return ESP_ERR_NO_MEM;

    esp_err_t err = i2s_dac_create(&(i2s_dac_config_t){
        .i2s_port = I2S_NUM_INTERNAL,
        .dac_gpio = GPIO_NUM_25,
    }, &s->dac);
    if (err != ESP_OK) { free(s); return err; }

    s->base.caps = BSP_AUDIO_CAP_PCM | BSP_AUDIO_CAP_SPEAKER;
    s->base.open            = b_open;
    s->base.close           = b_close;
    s->base.write           = b_write;
    s->base.set_hw_volume   = b_set_volume;
    s->base.set_hw_mute     = b_set_mute;
    s->base.get_dsp_profile = core_dsp_profile;
    s->base.deinit          = b_deinit;

    *out = &s->base;
    return ESP_OK;
}

esp_err_t core_audio_init(const bsp_config_t *config, axp192_handle_t axp) {
    bsp_audio_t *audio = NULL;
    esp_err_t err = axp ? core2_audio_create(axp, &audio)
                        : basic_audio_create(&audio);
    if (err != ESP_OK) return err;

    bsp_audio_set_active(audio, &(bsp_audio_init_t){
        .dsp_mode     = config->audio.dsp_mode,
        .speaker_mode = config->audio.speaker_mode,
    });
    return ESP_OK;
}
