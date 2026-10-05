/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_eq.h"
#include <math.h>
#include <stdlib.h>
#include <string.h>
#include "audf_alloc.h"
#include "audf_internal.h"
#include "audf_sample.h"
#include "audf_sync.h"

#ifndef M_PI
#define M_PI 3.14159265358979323846f
#endif

#define EQ_DEFAULT_MAX_STAGES 8
#define EQ_COEFF_Q            28
#define EQ_COEFF_HALF         ((int64_t)1 << (EQ_COEFF_Q - 1))

typedef struct {
    int32_t b0, b1, b2;
    int32_t a1, a2;
} biquad_q_t;

typedef struct {
    int64_t z1, z2;
} biquad_state_t;

struct audf_eq {
    audf_sync_t sync;
    audf_fmt_t  fmt;
    uint8_t     channels;
    size_t      max_stages;
    uint32_t    caps;

    biquad_q_t *pending;
    size_t      pending_stages;
    bool        pending_enabled;

    biquad_q_t     *active;
    size_t          active_stages;
    bool            active_enabled;
    biquad_state_t *states;   /* [max_stages * channels] */
};

static int32_t coeff_to_q(float c) {
    float v = c * (float)(1 << EQ_COEFF_Q);
    if (v >= 2147483647.0f) return INT32_MAX;
    if (v <= -2147483648.0f) return INT32_MIN;
    return (int32_t)lrintf(v);
}

esp_err_t audf_eq_create(const audf_eq_config_t *config, audf_eq_t **out) {
    if (!config || !out) return ESP_ERR_INVALID_ARG;
    if (config->channels < 1 || config->channels > AUDF_MAX_CHANNELS) return ESP_ERR_INVALID_ARG;

    audf_eq_t *eq = audf_calloc(1, sizeof(*eq), config->alloc_caps);
    if (!eq) return ESP_ERR_NO_MEM;
    eq->caps = config->alloc_caps;
    eq->fmt = config->fmt;
    eq->channels = config->channels;
    eq->max_stages = config->max_stages ? config->max_stages : EQ_DEFAULT_MAX_STAGES;
    eq->pending_enabled = config->enabled;
    eq->active_enabled = config->enabled;
    eq->pending = audf_calloc(eq->max_stages, sizeof(biquad_q_t), eq->caps);
    eq->active = audf_calloc(eq->max_stages, sizeof(biquad_q_t), eq->caps);
    eq->states = audf_calloc(eq->max_stages * eq->channels, sizeof(biquad_state_t), eq->caps);
    if (!eq->pending || !eq->active || !eq->states || audf_sync_init(&eq->sync) != ESP_OK) {
        audf_eq_destroy(eq);
        return ESP_ERR_NO_MEM;
    }
    *out = eq;
    return ESP_OK;
}

void audf_eq_destroy(audf_eq_t *eq) {
    if (!eq) return;
    audf_sync_deinit(&eq->sync);
    audf_free(eq->pending);
    audf_free(eq->active);
    audf_free(eq->states);
    audf_free(eq);
}

esp_err_t audf_eq_reconfig(audf_eq_t *eq, audf_fmt_t fmt, uint8_t channels) {
    if (!eq || channels < 1 || channels > AUDF_MAX_CHANNELS) return ESP_ERR_INVALID_ARG;
    if (channels != eq->channels) {
        biquad_state_t *states = audf_calloc(eq->max_stages * channels, sizeof(biquad_state_t), eq->caps);
        if (!states) return ESP_ERR_NO_MEM;
        audf_free(eq->states);
        eq->states = states;
        eq->channels = channels;
    } else {
        memset(eq->states, 0, eq->max_stages * channels * sizeof(biquad_state_t));
    }
    eq->fmt = fmt;
    return ESP_OK;
}

esp_err_t audf_eq_set_biquads(audf_eq_t *eq, const audf_biquad_t *biquads, size_t num_stages) {
    if (!eq || (num_stages && !biquads)) return ESP_ERR_INVALID_ARG;
    if (num_stages > eq->max_stages) return ESP_ERR_INVALID_SIZE;
    audf_sync_lock(&eq->sync);
    for (size_t i = 0; i < num_stages; i++) {
        eq->pending[i] = (biquad_q_t){
            .b0 = coeff_to_q(biquads[i].b0),
            .b1 = coeff_to_q(biquads[i].b1),
            .b2 = coeff_to_q(biquads[i].b2),
            .a1 = coeff_to_q(biquads[i].a1),
            .a2 = coeff_to_q(biquads[i].a2),
        };
    }
    eq->pending_stages = num_stages;
    audf_sync_publish(&eq->sync);
    return ESP_OK;
}

esp_err_t audf_eq_set_enabled(audf_eq_t *eq, bool enabled) {
    if (!eq) return ESP_ERR_INVALID_ARG;
    audf_sync_lock(&eq->sync);
    eq->pending_enabled = enabled;
    audf_sync_publish(&eq->sync);
    return ESP_OK;
}

bool audf_eq_get_enabled(audf_eq_t *eq) {
    if (!eq) return false;
    audf_sync_lock(&eq->sync);
    bool enabled = eq->pending_enabled;
    audf_sync_unlock(&eq->sync);
    return enabled;
}

audf_fmt_t audf_eq_fmt(const audf_eq_t *eq) { return eq->fmt; }
uint8_t audf_eq_channels(const audf_eq_t *eq) { return eq->channels; }

static void adopt(audf_eq_t *eq) {
    if (!audf_sync_try_adopt(&eq->sync)) return;
    size_t n = eq->pending_stages;
    memcpy(eq->active, eq->pending, n * sizeof(biquad_q_t));
    if (!eq->active_enabled && eq->pending_enabled) {
        memset(eq->states, 0, eq->max_stages * eq->channels * sizeof(biquad_state_t));
    } else if (n > eq->active_stages) {
        memset(&eq->states[eq->active_stages * eq->channels], 0,
               (n - eq->active_stages) * eq->channels * sizeof(biquad_state_t));
    }
    eq->active_stages = n;
    eq->active_enabled = eq->pending_enabled;
    audf_sync_unlock(&eq->sync);
}

AUDF_INLINE int32_t biquad_step(const biquad_q_t *b, biquad_state_t *s, int32_t x) {
    int64_t acc = (int64_t)b->b0 * x + s->z1;
    int32_t y = audf_sat32((acc + EQ_COEFF_HALF) >> EQ_COEFF_Q);
    s->z1 = (int64_t)b->b1 * x - (int64_t)b->a1 * y + s->z2;
    s->z2 = (int64_t)b->b2 * x - (int64_t)b->a2 * y;
    return y;
}

AUDF_INLINE void run(audf_eq_t *eq, audf_fmt_t fmt, const void *in, void *out, size_t frames) {
    const size_t channels = eq->channels;
    const size_t stages = eq->active_stages;
    for (size_t f = 0; f < frames; f++) {
        for (size_t ch = 0; ch < channels; ch++) {
            size_t i = f * channels + ch;
            int32_t x = audf_load(fmt, in, i);
            for (size_t s = 0; s < stages; s++) {
                x = biquad_step(&eq->active[s], &eq->states[s * channels + ch], x);
            }
            audf_store(fmt, out, i, x);
        }
    }
}

void audf_eq_process(audf_eq_t *eq, const void *in, void *out, size_t frames) {
    adopt(eq);
    if (!eq->active_enabled || !eq->active_stages) {
        if (in != out) memmove(out, in, frames * audf_frame_bytes(eq->fmt, eq->channels));
        return;
    }
    if (eq->fmt == AUDF_FMT_S16) {
        run(eq, AUDF_FMT_S16, in, out, frames);
    } else {
        run(eq, AUDF_FMT_S32, in, out, frames);
    }
}

static audf_biquad_t normalize(float b0, float b1, float b2, float a0, float a1, float a2) {
    float inv = 1.0f / a0;
    return (audf_biquad_t){
        .b0 = b0 * inv, .b1 = b1 * inv, .b2 = b2 * inv,
        .a1 = a1 * inv, .a2 = a2 * inv,
    };
}

audf_biquad_t audf_biquad_peaking(uint32_t fs, float f0, float q, float gain_db) {
    float A      = powf(10.0f, gain_db / 40.0f);
    float w0     = 2.0f * (float)M_PI * f0 / (float)fs;
    float cos_w0 = cosf(w0);
    float alpha  = sinf(w0) / (2.0f * q);
    return normalize(
        1.0f + alpha * A,
       -2.0f * cos_w0,
        1.0f - alpha * A,
        1.0f + alpha / A,
       -2.0f * cos_w0,
        1.0f - alpha / A);
}

audf_biquad_t audf_biquad_low_shelf(uint32_t fs, float f0, float q, float gain_db) {
    float A      = powf(10.0f, gain_db / 40.0f);
    float w0     = 2.0f * (float)M_PI * f0 / (float)fs;
    float cos_w0 = cosf(w0);
    float alpha  = sinf(w0) / (2.0f * q);
    float beta   = 2.0f * sqrtf(A) * alpha;
    return normalize(
            A * ((A + 1.0f) - (A - 1.0f) * cos_w0 + beta),
     2.0f * A * ((A - 1.0f) - (A + 1.0f) * cos_w0),
            A * ((A + 1.0f) - (A - 1.0f) * cos_w0 - beta),
                (A + 1.0f) + (A - 1.0f) * cos_w0 + beta,
        -2.0f * ((A - 1.0f) + (A + 1.0f) * cos_w0),
                (A + 1.0f) + (A - 1.0f) * cos_w0 - beta);
}

audf_biquad_t audf_biquad_high_shelf(uint32_t fs, float f0, float q, float gain_db) {
    float A      = powf(10.0f, gain_db / 40.0f);
    float w0     = 2.0f * (float)M_PI * f0 / (float)fs;
    float cos_w0 = cosf(w0);
    float alpha  = sinf(w0) / (2.0f * q);
    float beta   = 2.0f * sqrtf(A) * alpha;
    return normalize(
             A * ((A + 1.0f) + (A - 1.0f) * cos_w0 + beta),
     -2.0f * A * ((A - 1.0f) + (A + 1.0f) * cos_w0),
             A * ((A + 1.0f) + (A - 1.0f) * cos_w0 - beta),
                 (A + 1.0f) - (A - 1.0f) * cos_w0 + beta,
         2.0f * ((A - 1.0f) - (A + 1.0f) * cos_w0),
                 (A + 1.0f) - (A - 1.0f) * cos_w0 - beta);
}

audf_biquad_t audf_biquad_lowpass(uint32_t fs, float f0, float q) {
    float w0     = 2.0f * (float)M_PI * f0 / (float)fs;
    float cos_w0 = cosf(w0);
    float alpha  = sinf(w0) / (2.0f * q);
    float k      = (1.0f - cos_w0) * 0.5f;
    return normalize(
        k, 2.0f * k, k,
        1.0f + alpha, -2.0f * cos_w0, 1.0f - alpha);
}

audf_biquad_t audf_biquad_highpass(uint32_t fs, float f0, float q) {
    float w0     = 2.0f * (float)M_PI * f0 / (float)fs;
    float cos_w0 = cosf(w0);
    float alpha  = sinf(w0) / (2.0f * q);
    float k      = (1.0f + cos_w0) * 0.5f;
    return normalize(
        k, -2.0f * k, k,
        1.0f + alpha, -2.0f * cos_w0, 1.0f - alpha);
}
