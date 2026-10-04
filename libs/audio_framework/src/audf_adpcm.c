/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_adpcm.h"
#include <stdbool.h>
#include <stdlib.h>
#include "audf_codec_internal.h"

static const int8_t s_index_table[16] = {
    -1, -1, -1, -1, 2, 4, 6, 8,
    -1, -1, -1, -1, 2, 4, 6, 8,
};

static const int16_t s_step_table[89] = {
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17,
    19, 21, 23, 25, 28, 31, 34, 37, 41, 45,
    50, 55, 60, 66, 73, 80, 88, 97, 107, 118,
    130, 143, 157, 173, 190, 209, 230, 253, 279, 307,
    337, 371, 408, 449, 494, 544, 598, 658, 724, 796,
    876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066,
    2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358,
    5894, 6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899,
    15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
};

typedef struct {
    int32_t predictor;
    int32_t index;
} channel_state_t;

static int16_t expand(channel_state_t *s, uint8_t nibble) {
    int32_t step = s_step_table[s->index];
    int32_t diff = step >> 3;
    if (nibble & 1) diff += step >> 2;
    if (nibble & 2) diff += step >> 1;
    if (nibble & 4) diff += step;
    s->predictor += (nibble & 8) ? -diff : diff;
    if (s->predictor > INT16_MAX) s->predictor = INT16_MAX;
    if (s->predictor < INT16_MIN) s->predictor = INT16_MIN;
    s->index += s_index_table[nibble];
    if (s->index < 0) s->index = 0;
    if (s->index > 88) s->index = 88;
    return (int16_t)s->predictor;
}

static uint8_t compress(channel_state_t *s, int16_t sample) {
    int32_t step = s_step_table[s->index];
    int32_t diff = sample - s->predictor;
    uint8_t nibble = 0;
    if (diff < 0) {
        nibble = 8;
        diff = -diff;
    }
    for (uint8_t mask = 4; mask; mask >>= 1) {
        if (diff >= step) {
            nibble |= mask;
            diff -= step;
        }
        step >>= 1;
    }
    expand(s, nibble);
    return nibble;
}

static bool config_valid(const audf_adpcm_config_t *config) {
    if (!config || config->channels < 1 || config->channels > AUDF_MAX_CHANNELS) return false;
    size_t group = 4u * config->channels;
    return config->block_align > group && config->block_align % group == 0;
}

size_t audf_adpcm_block_frames(const audf_adpcm_config_t *config) {
    if (!config_valid(config)) return 0;
    return (size_t)(config->block_align - 4u * config->channels) * 2 / config->channels + 1;
}

static esp_err_t decode(audf_decoder_t *dec, const void *frame, size_t len, void *pcm, size_t *frames) {
    const uint8_t *src = frame;
    int16_t *out = pcm;
    const size_t channels = dec->channels;
    const size_t group = 4 * channels;
    if (len < group || len % group || (len - group) / group * 8 + 1 > dec->max_frames) {
        return ESP_ERR_INVALID_SIZE;
    }
    channel_state_t state[AUDF_MAX_CHANNELS];
    for (size_t ch = 0; ch < channels; ch++) {
        const uint8_t *h = src + ch * 4;
        state[ch].predictor = (int16_t)((uint16_t)h[0] | (uint16_t)h[1] << 8);
        state[ch].index = h[2] > 88 ? 88 : h[2];
        out[ch] = (int16_t)state[ch].predictor;
    }
    size_t groups = (len - group) / group;
    const uint8_t *data = src + group;
    for (size_t g = 0; g < groups; g++) {
        for (size_t ch = 0; ch < channels; ch++) {
            const uint8_t *bytes = data + (g * channels + ch) * 4;
            for (size_t b = 0; b < 4; b++) {
                size_t f = 1 + g * 8 + b * 2;
                out[f * channels + ch] = expand(&state[ch], bytes[b] & 0x0f);
                out[(f + 1) * channels + ch] = expand(&state[ch], bytes[b] >> 4);
            }
        }
    }
    *frames = 1 + groups * 8;
    return ESP_OK;
}

static void decoder_destroy(audf_decoder_t *dec) {
    free(dec);
}

static const audf_decoder_ops_t s_decoder_ops = {
    .decode = decode,
    .destroy = decoder_destroy,
};

esp_err_t audf_adpcm_decoder_create(const audf_adpcm_config_t *config, audf_decoder_t **out) {
    if (!out || !config_valid(config)) return ESP_ERR_INVALID_ARG;
    audf_decoder_t *dec = calloc(1, sizeof(*dec));
    if (!dec) return ESP_ERR_NO_MEM;
    dec->ops = &s_decoder_ops;
    dec->fmt = AUDF_FMT_S16;
    dec->channels = config->channels;
    dec->max_frames = audf_adpcm_block_frames(config);
    *out = dec;
    return ESP_OK;
}

typedef struct {
    audf_encoder_t base;
    channel_state_t state[AUDF_MAX_CHANNELS];
} adpcm_encoder_t;

static esp_err_t encode(audf_encoder_t *enc, const void *pcm, size_t frames, void *out, size_t *len) {
    adpcm_encoder_t *e = (adpcm_encoder_t *)enc;
    const int16_t *in = pcm;
    uint8_t *dst = out;
    const size_t channels = enc->channels;
    size_t groups = (frames - 1 + 7) / 8;
    for (size_t ch = 0; ch < channels; ch++) {
        int16_t first = in[ch];
        e->state[ch].predictor = first;
        dst[ch * 4 + 0] = (uint8_t)first;
        dst[ch * 4 + 1] = (uint8_t)((uint16_t)first >> 8);
        dst[ch * 4 + 2] = (uint8_t)e->state[ch].index;
        dst[ch * 4 + 3] = 0;
    }
    uint8_t *data = dst + 4 * channels;
    for (size_t g = 0; g < groups; g++) {
        for (size_t ch = 0; ch < channels; ch++) {
            uint8_t *bytes = data + (g * channels + ch) * 4;
            for (size_t b = 0; b < 4; b++) {
                size_t f = 1 + g * 8 + b * 2;
                size_t f0 = f < frames ? f : frames - 1;
                size_t f1 = f + 1 < frames ? f + 1 : frames - 1;
                uint8_t lo = compress(&e->state[ch], in[f0 * channels + ch]);
                uint8_t hi = compress(&e->state[ch], in[f1 * channels + ch]);
                bytes[b] = (uint8_t)(lo | hi << 4);
            }
        }
    }
    *len = 4 * channels * (1 + groups);
    return ESP_OK;
}

static void encoder_destroy(audf_encoder_t *enc) {
    free(enc);
}

static const audf_encoder_ops_t s_encoder_ops = {
    .encode = encode,
    .destroy = encoder_destroy,
};

esp_err_t audf_adpcm_encoder_create(const audf_adpcm_config_t *config, audf_encoder_t **out) {
    if (!out || !config_valid(config)) return ESP_ERR_INVALID_ARG;
    adpcm_encoder_t *e = calloc(1, sizeof(*e));
    if (!e) return ESP_ERR_NO_MEM;
    e->base.ops = &s_encoder_ops;
    e->base.fmt = AUDF_FMT_S16;
    e->base.channels = config->channels;
    e->base.frame_frames = audf_adpcm_block_frames(config);
    e->base.max_bytes = config->block_align;
    *out = &e->base;
    return ESP_OK;
}
