/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_gain.h"
#include <math.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>
#include "audf_alloc.h"
#include "audf_internal.h"
#include "audf_sample.h"
#include "audf_sync.h"

#define GAIN_Q      24
#define GAIN_UNITY  (1 << GAIN_Q)
#define GAIN_HALF   ((int64_t)1 << (GAIN_Q - 1))

struct audf_gain {
    audf_sync_t sync;
    audf_fmt_t  fmt;
    uint8_t     channels;
    uint32_t    sample_rate;

    float    target;
    int32_t  pending_target_q;
    uint32_t pending_fade_ms;
    bool     pending_jump;
    int32_t  pending_jump_q;

    int32_t  current_q;
    int32_t  target_q;
    int32_t  step_q;
    uint32_t fade_remaining;
};

static float clamp_gain(float g) {
    if (!(g > 0.0f)) return 0.0f;
    return g > AUDF_GAIN_MAX ? AUDF_GAIN_MAX : g;
}

static int32_t gain_to_q(float g) {
    return (int32_t)lrintf(g * (float)GAIN_UNITY);
}

esp_err_t audf_gain_create(const audf_gain_config_t *config, audf_gain_t **out) {
    if (!config || !out || !config->sample_rate) return ESP_ERR_INVALID_ARG;
    if (config->channels < 1 || config->channels > AUDF_MAX_CHANNELS) return ESP_ERR_INVALID_ARG;
    audf_gain_t *gain = audf_calloc(1, sizeof(*gain), config->alloc_caps);
    if (!gain) return ESP_ERR_NO_MEM;
    if (audf_sync_init(&gain->sync) != ESP_OK) {
        audf_free(gain);
        return ESP_ERR_NO_MEM;
    }
    gain->fmt = config->fmt;
    gain->channels = config->channels;
    gain->sample_rate = config->sample_rate;
    gain->target = clamp_gain(config->gain);
    gain->current_q = gain->target_q = gain->pending_target_q = gain_to_q(gain->target);
    *out = gain;
    return ESP_OK;
}

void audf_gain_destroy(audf_gain_t *gain) {
    if (!gain) return;
    audf_sync_deinit(&gain->sync);
    audf_free(gain);
}

esp_err_t audf_gain_reconfig(audf_gain_t *gain, audf_fmt_t fmt, uint8_t channels, uint32_t sample_rate) {
    if (!gain || !sample_rate || channels < 1 || channels > AUDF_MAX_CHANNELS) return ESP_ERR_INVALID_ARG;
    gain->fmt = fmt;
    gain->channels = channels;
    gain->sample_rate = sample_rate;
    return ESP_OK;
}

esp_err_t audf_gain_set(audf_gain_t *gain, float target, uint32_t fade_ms) {
    if (!gain) return ESP_ERR_INVALID_ARG;
    target = clamp_gain(target);
    audf_sync_lock(&gain->sync);
    gain->target = target;
    gain->pending_target_q = gain_to_q(target);
    gain->pending_fade_ms = fade_ms;
    if (!fade_ms) {
        gain->pending_jump = true;
        gain->pending_jump_q = gain->pending_target_q;
    }
    audf_sync_publish(&gain->sync);
    return ESP_OK;
}

float audf_gain_get(audf_gain_t *gain) {
    if (!gain) return 0.0f;
    audf_sync_lock(&gain->sync);
    float target = gain->target;
    audf_sync_unlock(&gain->sync);
    return target;
}

audf_fmt_t audf_gain_fmt(const audf_gain_t *gain) { return gain->fmt; }
uint8_t audf_gain_channels(const audf_gain_t *gain) { return gain->channels; }

static void adopt(audf_gain_t *gain) {
    if (!audf_sync_try_adopt(&gain->sync)) return;
    int32_t target_q = gain->pending_target_q;
    uint32_t fade_ms = gain->pending_fade_ms;
    if (gain->pending_jump) {
        gain->current_q = gain->pending_jump_q;
        gain->pending_jump = false;
    }
    audf_sync_unlock(&gain->sync);

    gain->target_q = target_q;
    uint32_t frames = (uint32_t)(((uint64_t)gain->sample_rate * fade_ms + 500) / 1000);
    if (!frames || gain->current_q == target_q) {
        gain->current_q = target_q;
        gain->fade_remaining = 0;
        gain->step_q = 0;
    } else {
        gain->fade_remaining = frames;
        gain->step_q = (target_q - gain->current_q) / (int32_t)frames;
    }
}

AUDF_INLINE void run(audf_gain_t *gain, audf_fmt_t fmt, const void *in, void *out, size_t frames) {
    const size_t channels = gain->channels;
    int32_t g = gain->current_q;
    uint32_t remaining = gain->fade_remaining;
    for (size_t f = 0; f < frames; f++) {
        if (remaining) {
            g += gain->step_q;
            if (--remaining == 0) g = gain->target_q;
        }
        for (size_t ch = 0; ch < channels; ch++) {
            size_t i = f * channels + ch;
            int64_t x = audf_load(fmt, in, i);
            audf_store(fmt, out, i, (x * g + GAIN_HALF) >> GAIN_Q);
        }
    }
    gain->current_q = g;
    gain->fade_remaining = remaining;
}

void audf_gain_process(audf_gain_t *gain, const void *in, void *out, size_t frames) {
    adopt(gain);
    if (gain->current_q == GAIN_UNITY && !gain->fade_remaining) {
        if (in != out) memmove(out, in, frames * audf_frame_bytes(gain->fmt, gain->channels));
        return;
    }
    if (gain->fmt == AUDF_FMT_S16) {
        run(gain, AUDF_FMT_S16, in, out, frames);
    } else {
        run(gain, AUDF_FMT_S32, in, out, frames);
    }
}
