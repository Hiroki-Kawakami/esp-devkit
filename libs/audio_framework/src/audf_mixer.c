/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_mixer.h"
#include <math.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>
#include "audf_alloc.h"
#include "audf_internal.h"
#include "audf_sample.h"
#include "audf_sync.h"

#define MIX_Q     24
#define MIX_UNITY (1 << MIX_Q)
#define MIX_HALF  ((int64_t)1 << (MIX_Q - 1))
#define MIX_MAX   127.0f

typedef struct {
    uint8_t  channels;
    int32_t *pending;   /* [out_channels * channels] */
    int32_t *active;
    bool     active_identity;
} mix_input_t;

struct audf_mixer {
    audf_sync_t  sync;
    audf_fmt_t   fmt;
    uint8_t      out_channels;
    uint8_t      num_inputs;
    mix_input_t *inputs;
};

static int32_t coeff_to_q(float c) {
    if (c > MIX_MAX) c = MIX_MAX;
    if (c < -MIX_MAX) c = -MIX_MAX;
    return (int32_t)lrintf(c * (float)MIX_UNITY);
}

static void default_matrix(int32_t *m, uint8_t out_ch, uint8_t in_ch) {
    memset(m, 0, (size_t)out_ch * in_ch * sizeof(int32_t));
    for (uint8_t o = 0; o < out_ch; o++) {
        if (in_ch == 1) {
            m[o] = MIX_UNITY;
        } else if (out_ch == 1) {
            for (uint8_t i = 0; i < in_ch; i++) m[i] = MIX_UNITY / in_ch;
        } else if (o < in_ch) {
            m[o * in_ch + o] = MIX_UNITY;
        }
    }
}

static bool is_identity(const int32_t *m, uint8_t out_ch, uint8_t in_ch) {
    if (out_ch != in_ch) return false;
    for (uint8_t o = 0; o < out_ch; o++) {
        for (uint8_t i = 0; i < in_ch; i++) {
            if (m[o * in_ch + i] != (o == i ? MIX_UNITY : 0)) return false;
        }
    }
    return true;
}

esp_err_t audf_mixer_create(const audf_mixer_config_t *config, audf_mixer_t **out) {
    if (!config || !out || !config->num_inputs || !config->in_channels) return ESP_ERR_INVALID_ARG;
    if (config->out_channels < 1 || config->out_channels > AUDF_MAX_CHANNELS) return ESP_ERR_INVALID_ARG;
    for (uint8_t n = 0; n < config->num_inputs; n++) {
        uint8_t ch = config->in_channels[n];
        if (ch < 1 || ch > AUDF_MAX_CHANNELS) return ESP_ERR_INVALID_ARG;
    }

    audf_mixer_t *mixer = audf_calloc(1, sizeof(*mixer), config->alloc_caps);
    if (!mixer) return ESP_ERR_NO_MEM;
    mixer->fmt = config->fmt;
    mixer->out_channels = config->out_channels;
    mixer->num_inputs = config->num_inputs;
    mixer->inputs = audf_calloc(config->num_inputs, sizeof(mix_input_t), config->alloc_caps);
    if (!mixer->inputs || audf_sync_init(&mixer->sync) != ESP_OK) {
        audf_mixer_destroy(mixer);
        return ESP_ERR_NO_MEM;
    }
    for (uint8_t n = 0; n < config->num_inputs; n++) {
        mix_input_t *in = &mixer->inputs[n];
        size_t count = (size_t)config->out_channels * config->in_channels[n];
        in->channels = config->in_channels[n];
        in->pending = audf_malloc(count * sizeof(int32_t), config->alloc_caps);
        in->active = audf_malloc(count * sizeof(int32_t), config->alloc_caps);
        if (!in->pending || !in->active) {
            audf_mixer_destroy(mixer);
            return ESP_ERR_NO_MEM;
        }
        default_matrix(in->pending, config->out_channels, in->channels);
        memcpy(in->active, in->pending, count * sizeof(int32_t));
        in->active_identity = is_identity(in->active, config->out_channels, in->channels);
    }
    *out = mixer;
    return ESP_OK;
}

void audf_mixer_destroy(audf_mixer_t *mixer) {
    if (!mixer) return;
    audf_sync_deinit(&mixer->sync);
    if (mixer->inputs) {
        for (uint8_t n = 0; n < mixer->num_inputs; n++) {
            audf_free(mixer->inputs[n].pending);
            audf_free(mixer->inputs[n].active);
        }
    }
    audf_free(mixer->inputs);
    audf_free(mixer);
}

esp_err_t audf_mixer_set_matrix(audf_mixer_t *mixer, uint8_t input, const float *matrix) {
    if (!mixer || !matrix || input >= mixer->num_inputs) return ESP_ERR_INVALID_ARG;
    mix_input_t *in = &mixer->inputs[input];
    size_t count = (size_t)mixer->out_channels * in->channels;
    audf_sync_lock(&mixer->sync);
    for (size_t i = 0; i < count; i++) in->pending[i] = coeff_to_q(matrix[i]);
    audf_sync_publish(&mixer->sync);
    return ESP_OK;
}

esp_err_t audf_mixer_get_matrix(audf_mixer_t *mixer, uint8_t input, float *matrix) {
    if (!mixer || !matrix || input >= mixer->num_inputs) return ESP_ERR_INVALID_ARG;
    mix_input_t *in = &mixer->inputs[input];
    size_t count = (size_t)mixer->out_channels * in->channels;
    audf_sync_lock(&mixer->sync);
    for (size_t i = 0; i < count; i++) matrix[i] = (float)in->pending[i] / (float)MIX_UNITY;
    audf_sync_unlock(&mixer->sync);
    return ESP_OK;
}

uint8_t audf_mixer_num_inputs(const audf_mixer_t *mixer) { return mixer->num_inputs; }
audf_fmt_t audf_mixer_fmt(const audf_mixer_t *mixer) { return mixer->fmt; }
uint8_t audf_mixer_out_channels(const audf_mixer_t *mixer) { return mixer->out_channels; }
uint8_t audf_mixer_in_channels(const audf_mixer_t *mixer, uint8_t input) { return mixer->inputs[input].channels; }

static void adopt(audf_mixer_t *mixer) {
    if (!audf_sync_try_adopt(&mixer->sync)) return;
    for (uint8_t n = 0; n < mixer->num_inputs; n++) {
        mix_input_t *in = &mixer->inputs[n];
        memcpy(in->active, in->pending, (size_t)mixer->out_channels * in->channels * sizeof(int32_t));
        in->active_identity = is_identity(in->active, mixer->out_channels, in->channels);
    }
    audf_sync_unlock(&mixer->sync);
}

AUDF_INLINE void run(audf_mixer_t *mixer, audf_fmt_t fmt, const void *const in[], void *out, size_t frames) {
    const uint8_t out_ch = mixer->out_channels;
    for (size_t f = 0; f < frames; f++) {
        int64_t acc[AUDF_MAX_CHANNELS] = {0};
        for (uint8_t n = 0; n < mixer->num_inputs; n++) {
            if (!in[n]) continue;
            const mix_input_t *mi = &mixer->inputs[n];
            const uint8_t in_ch = mi->channels;
            int32_t x[AUDF_MAX_CHANNELS];
            for (uint8_t i = 0; i < in_ch; i++) x[i] = audf_load(fmt, in[n], f * in_ch + i);
            const int32_t *m = mi->active;
            for (uint8_t o = 0; o < out_ch; o++) {
                for (uint8_t i = 0; i < in_ch; i++) acc[o] += (int64_t)m[o * in_ch + i] * x[i];
            }
        }
        for (uint8_t o = 0; o < out_ch; o++) {
            audf_store(fmt, out, f * out_ch + o, (acc[o] + MIX_HALF) >> MIX_Q);
        }
    }
}

void audf_mixer_process(audf_mixer_t *mixer, const void *const in[], void *out, size_t frames) {
    adopt(mixer);
    if (mixer->num_inputs == 1 && in[0] && mixer->inputs[0].active_identity) {
        if (in[0] != out) memmove(out, in[0], frames * audf_frame_bytes(mixer->fmt, mixer->out_channels));
        return;
    }
    if (mixer->fmt == AUDF_FMT_S16) {
        run(mixer, AUDF_FMT_S16, in, out, frames);
    } else {
        run(mixer, AUDF_FMT_S32, in, out, frames);
    }
}
