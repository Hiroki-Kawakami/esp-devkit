/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "core_s3_audio.h"
#include <stdlib.h>
#include "aw88298/aw88298.h"

#define I2S_PIN_BCLK   GPIO_NUM_34
#define I2S_PIN_WS     GPIO_NUM_33
#define I2S_PIN_DOUT   GPIO_NUM_13
#define SPK_EN_PIN     2   /* AW9523B P0_2 */

typedef struct {
    bsp_audio_t base;
    aw88298_t codec;
    aw9523_t  io;
} core_s3_audio_state_t;

static esp_err_t stream_open(bsp_audio_t *self, uint32_t rate, uint8_t bits, uint8_t ch) {
    return aw88298_open(((core_s3_audio_state_t *)self)->codec, rate, bits, ch);
}

static esp_err_t stream_close(bsp_audio_t *self) {
    return aw88298_close(((core_s3_audio_state_t *)self)->codec);
}

static esp_err_t stream_write(bsp_audio_t *self, const void *data, size_t len) {
    return aw88298_write(((core_s3_audio_state_t *)self)->codec, data, len);
}

static esp_err_t set_hw_volume(bsp_audio_t *self, int volume) {
    return aw88298_set_volume(((core_s3_audio_state_t *)self)->codec, volume);
}

static esp_err_t set_hw_mute(bsp_audio_t *self, bool mute) {
    return aw88298_set_mute(((core_s3_audio_state_t *)self)->codec, mute);
}

static esp_err_t set_speaker_enabled(bsp_audio_t *self, bool enabled) {
    return aw9523_set_output(((core_s3_audio_state_t *)self)->io, SPK_EN_PIN, enabled);
}

/* Mono speaker voicing for the small internal driver: high-pass out the sub,
 * lift the low-mid body, downmix to mono. */
static esp_err_t get_dsp_profile(bsp_audio_t *self, bool headphone, uint32_t sample_rate,
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

static esp_err_t deinit(bsp_audio_t *self) {
    core_s3_audio_state_t *s = (core_s3_audio_state_t *)self;
    if (s->codec) aw88298_deinit(s->codec);
    free(s);
    return ESP_OK;
}

esp_err_t core_s3_audio_create(const core_s3_audio_config_t *config, bsp_audio_t **out_audio) {
    if (!config || !out_audio || !config->i2c_bus || !config->io_expander) {
        return ESP_ERR_INVALID_ARG;
    }

    core_s3_audio_state_t *s = calloc(1, sizeof(*s));
    if (!s) return ESP_ERR_NO_MEM;

    esp_err_t err = aw88298_init(&(aw88298_config_t){
        .i2c_bus   = config->i2c_bus,
        .i2s_port  = I2S_NUM_1,
        .bclk_gpio = I2S_PIN_BCLK,
        .ws_gpio   = I2S_PIN_WS,
        .dout_gpio = I2S_PIN_DOUT,
    }, &s->codec);
    if (err != ESP_OK) {
        free(s);
        return err;
    }
    s->io = config->io_expander;

    s->base.caps = BSP_AUDIO_CAP_PCM | BSP_AUDIO_CAP_SPEAKER;
    s->base.open                = stream_open;
    s->base.close               = stream_close;
    s->base.write               = stream_write;
    s->base.set_hw_volume       = set_hw_volume;
    s->base.set_hw_mute         = set_hw_mute;
    s->base.set_speaker_enabled = set_speaker_enabled;
    s->base.get_dsp_profile     = get_dsp_profile;
    s->base.deinit              = deinit;

    *out_audio = &s->base;
    return ESP_OK;
}
