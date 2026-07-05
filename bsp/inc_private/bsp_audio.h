/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Internal audio-driver interface. Same struct-inheritance vtable as
 * bsp_display / bsp_touch: a provider fills `caps` and the ops its hardware
 * has (the rest stay NULL) and registers CLOSED — no signal output, amp gate
 * off until the first public open. All policy lives in the dispatch
 * (src/bsp_audio.c): caps gating, the volume curve delivered as a fading
 * software gain, DSP voicing, the speaker route policy + headphone poll
 * task, and the tone synth fallback (CAP_PCM without CAP_TONE).
 *
 * Click-free contract: the amp gate (set_speaker_enabled) only switches while
 * the DAC output is settled silence, and audible amplitude changes go through
 * the software gain fade — set_hw_volume / set_hw_mute are reserved for power
 * transitions where the output is already silent.
 */

#pragma once
#include "bsp.h"

typedef struct bsp_audio bsp_audio_t;

/* A board's DSP tuning for one output route, designed at `sample_rate`;
 * re-queried on open and — in DSP_MODE_AUTO — on HP insert/remove. */
#define BSP_AUDIO_DSP_PROFILE_MAX_STAGES 8
typedef struct {
    audio_dsp_biquad_t biquads[BSP_AUDIO_DSP_PROFILE_MAX_STAGES];
    size_t num_stages;
    bool eq_enabled;
    bool mono_mix;
} bsp_audio_dsp_profile_t;

struct bsp_audio {
    uint32_t caps;   /* BSP_AUDIO_CAP_* */

    /* PCM playback — required when CAP_PCM is set. open() may also be called
     * on a running stream with a new format; how to reconfigure (light reclock
     * vs full reopen) is the provider's choice. write() blocks while the
     * output pipeline is full (the public API's pacing). */
    esp_err_t (*open)(bsp_audio_t *self, uint32_t sample_rate, uint8_t bits_per_sample, uint8_t channels);
    esp_err_t (*close)(bsp_audio_t *self);
    esp_err_t (*write)(bsp_audio_t *self, const void *data, size_t len);

    /* Optional; power-transition use only (see the click-free contract). */
    esp_err_t (*set_hw_volume)(bsp_audio_t *self, int volume);   /* 0..100 */
    esp_err_t (*set_hw_mute)(bsp_audio_t *self, bool mute);

    /* Routing — NULL when the hardware has no such control. */
    esp_err_t (*set_speaker_enabled)(bsp_audio_t *self, bool enabled);     /* amp gate */
    esp_err_t (*headphone_inserted)(bsp_audio_t *self, bool *inserted);    /* CAP_HEADPHONE */

    /* Board DSP tuning per route — optional (NULL = flat in every mode). */
    esp_err_t (*get_dsp_profile)(bsp_audio_t *self, bool headphone, uint32_t sample_rate,
                                 bsp_audio_dsp_profile_t *out);

    /* Buzzer — required when CAP_TONE is set. */
    esp_err_t (*tone)(bsp_audio_t *self, uint32_t freq_hz, uint32_t duration_ms);
    esp_err_t (*tone_stop)(bsp_audio_t *self);

    esp_err_t (*deinit)(bsp_audio_t *self);
};

/* Dispatch-layer policy selection, from bsp_config_t.audio. */
typedef struct {
    bsp_audio_dsp_mode_t dsp_mode;
    bsp_audio_speaker_mode_t speaker_mode;
} bsp_audio_init_t;

/* Board's bsp_init() calls this once after creating its (closed) provider;
 * NULL deactivates audio. */
void bsp_audio_set_active(bsp_audio_t *audio, const bsp_audio_init_t *init);

/* Silence the audio path for a power transition (hw mute + amp off + tone
 * stop) — called by bsp_power_restart() / bsp_power_off(). Safe with no provider. */
void bsp_audio_quiesce(void);
