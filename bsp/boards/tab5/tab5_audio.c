/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "tab5_audio.h"
#include <stdlib.h>
#include "es8388/es8388.h"
#include "driver/gpio.h"

#define SPK_EN_PIN  1
#define HP_DET_PIN  7

typedef struct {
    bsp_audio_t base;
    es8388_t codec;
    pi4io_t io;
} tab5_audio_state_t;

static esp_err_t stream_open(bsp_audio_t *self, uint32_t rate, uint8_t bits, uint8_t ch) {
    tab5_audio_state_t *s = (tab5_audio_state_t *)self;
    return es8388_open(s->codec, rate, bits, ch);
}

static esp_err_t stream_close(bsp_audio_t *self) {
    tab5_audio_state_t *s = (tab5_audio_state_t *)self;
    return es8388_close(s->codec);
}

static esp_err_t stream_write(bsp_audio_t *self, const void *data, size_t len) {
    tab5_audio_state_t *s = (tab5_audio_state_t *)self;
    return es8388_write(s->codec, data, len);
}

static esp_err_t set_hw_volume(bsp_audio_t *self, int volume) {
    tab5_audio_state_t *s = (tab5_audio_state_t *)self;
    return es8388_set_volume(s->codec, volume);
}

static esp_err_t set_hw_mute(bsp_audio_t *self, bool mute) {
    tab5_audio_state_t *s = (tab5_audio_state_t *)self;
    return es8388_set_mute(s->codec, mute);
}

static esp_err_t set_speaker_enabled(bsp_audio_t *self, bool enabled) {
    tab5_audio_state_t *s = (tab5_audio_state_t *)self;
    return pi4io_set_output(s->io, SPK_EN_PIN, enabled);
}

static esp_err_t headphone_inserted(bsp_audio_t *self, bool *inserted) {
    tab5_audio_state_t *s = (tab5_audio_state_t *)self;
    /* HP_DET is pulled up externally; the jack's NC detect switch shorts the
     * line to GND when no plug is inserted, so HP_DET=1 => headphones in. */
    return pi4io_get_input(s->io, HP_DET_PIN, inserted);
}

/* Tab5 output voicing (coefficients designed at the live stream rate). Speaker
 * is LOUT1 only (mono) — downmix and shape for the small driver; headphone is
 * LOUT1/ROUT1 stereo, flat-ish. */
static esp_err_t get_dsp_profile(bsp_audio_t *self, bool headphone, uint32_t sample_rate,
                                 bsp_audio_dsp_profile_t *out) {
    (void)self;
    const uint32_t fs = sample_rate ? sample_rate : 48000;
    if (headphone) {
        out->biquads[0] = audio_dsp_design_highpass (fs,   50.0f, 0.707f);
        out->biquads[1] = audio_dsp_design_low_shelf(fs,  150.0f, 0.707f, +10.0f);
        out->biquads[2] = audio_dsp_design_peaking  (fs, 1000.0f, 0.80f,  -4.0f);
        out->biquads[3] = audio_dsp_design_peaking  (fs, 2500.0f, 1.00f,  -3.0f);
        out->num_stages = 4;
        out->mono_mix   = false;
    } else {
        out->biquads[0] = audio_dsp_design_highpass (fs,  80.0f, 0.707f);
        out->biquads[1] = audio_dsp_design_low_shelf(fs, 300.0f, 0.707f, +7.0f);
        out->biquads[2] = audio_dsp_design_peaking  (fs, 150.0f, 1.20f,  +3.0f);
        out->num_stages = 3;
        out->mono_mix   = true;
    }
    out->eq_enabled = true;
    return ESP_OK;
}

static esp_err_t deinit(bsp_audio_t *self) {
    tab5_audio_state_t *s = (tab5_audio_state_t *)self;
    if (s->codec) es8388_deinit(s->codec);
    free(s);
    return ESP_OK;
}

esp_err_t tab5_audio_create(const tab5_audio_config_t *config, bsp_audio_t **out_audio) {
    if (!config || !out_audio || !config->i2c_bus || !config->io_expander) {
        return ESP_ERR_INVALID_ARG;
    }

    tab5_audio_state_t *s = calloc(1, sizeof(*s));
    if (!s) return ESP_ERR_NO_MEM;

    esp_err_t err = es8388_init(&(es8388_config_t){
        .i2c_bus     = config->i2c_bus,
        .i2s_port    = I2S_NUM_0,
        .mclk_gpio   = GPIO_NUM_30,
        .bclk_gpio   = GPIO_NUM_27,
        .ws_gpio     = GPIO_NUM_29,
        .dout_gpio   = GPIO_NUM_26,
        .din_gpio    = GPIO_NUM_28,
        .dac_outputs = ES8388_OUT_LOUT1 | ES8388_OUT_ROUT1 |
                       ES8388_OUT_LOUT2 | ES8388_OUT_ROUT2,  /* enable all DAC outs (HP right needs ROUT2) */
    }, &s->codec);
    if (err != ESP_OK) {
        free(s);
        return err;
    }
    s->io = config->io_expander;

    s->base.caps = BSP_AUDIO_CAP_PCM | BSP_AUDIO_CAP_SPEAKER | BSP_AUDIO_CAP_HEADPHONE;
    s->base.open                = stream_open;
    s->base.close               = stream_close;
    s->base.write               = stream_write;
    s->base.set_hw_volume       = set_hw_volume;
    s->base.set_hw_mute         = set_hw_mute;
    s->base.set_speaker_enabled = set_speaker_enabled;
    s->base.headphone_inserted  = headphone_inserted;
    s->base.get_dsp_profile     = get_dsp_profile;
    s->base.deinit              = deinit;

    *out_audio = &s->base;
    return ESP_OK;
}
