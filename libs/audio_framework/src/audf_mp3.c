/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <string.h>

#include "audf_alloc.h"
#include "audf_mp3_internal.h"
#include "audf_mp3_kernels.h"

#define SYNTH_FRAC   23
#define SYNTH_IN_MAX ((1 << 26) - 1)
#define SYNTH_V_MAX  (1 << 28)
#define SYNTH_V_FRAC (SYNTH_FRAC + 1)
/* Half of the 22 window rows, whose low products each round down. */
#define SYNTH_FLOOR_BIAS 11

static const uint16_t s_kbps[3][15] = {
    { 0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384 },
    { 0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320 },
    { 0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160 },
};

static const uint32_t s_rates[3] = { 44100, 48000, 32000 };

static bool parse(const uint8_t *p, mp3_frame_t *f) {
    if (p[0] != 0xff || (p[1] & 0xe0) != 0xe0) return false;
    const unsigned version_bits = (p[1] >> 3) & 3;
    const unsigned layer_bits = (p[1] >> 1) & 3;
    const unsigned bitrate = p[2] >> 4;
    const unsigned rate = (p[2] >> 2) & 3;
    if (version_bits == 1 || (layer_bits != 1 && layer_bits != 2)) return false;
    if (bitrate == 0 || bitrate == 15 || rate == 3) return false;

    const uint8_t version = version_bits == 3 ? 0 : version_bits == 2 ? 1 : 2;
    const uint8_t layer = layer_bits == 2 ? 2 : 3;
    if (version == 2 && layer == 2) return false;

    f->version = version;
    f->rate_index = (uint8_t)(version * 3 + rate);
    f->kbps = s_kbps[version ? 2 : layer == 2 ? 0 : 1][bitrate];
    f->mode = p[3] >> 6;
    f->mode_ext = (p[3] >> 4) & 3;
    f->crc = !(p[1] & 1);
    f->h.layer = layer;
    f->h.rate = s_rates[rate] >> version;
    f->h.channels = f->mode == MP3_MONO ? 1 : 2;
    f->h.frames = layer == 3 && version ? 576 : 1152;
    const uint32_t per = f->h.frames / 8;
    f->h.bytes = (uint16_t)(per * 1000u * f->kbps / f->h.rate + ((p[2] >> 1) & 1));
    return true;
}

bool audf_mp3_parse_header(const uint8_t *p, audf_mp3_header_t *out) {
    mp3_frame_t f;
    if (!p || !out || !parse(p, &f)) return false;
    *out = f.h;
    return true;
}

static inline int32_t to_v(int32_t v) {
    return (v > SYNTH_V_MAX ? SYNTH_V_MAX : v < -SYNTH_V_MAX ? -SYNTH_V_MAX : v) * (1 << (SYNTH_V_FRAC - SYNTH_FRAC));
}

static void synth(mp3_synth_t *s, const int32_t *in, int16_t *out, int stride) {
    int32_t x[32];
    uint32_t mag = 0;
    for (int k = 0; k < 32; k++) {
        const int32_t v = (in[k] + (1 << (MP3_FRAC - SYNTH_FRAC - 1))) >> (MP3_FRAC - SYNTH_FRAC);
        x[k] = v > SYNTH_IN_MAX ? SYNTH_IN_MAX : v < -SYNTH_IN_MAX ? -SYNTH_IN_MAX : v;
        mag |= (uint32_t)(x[k] < 0 ? -x[k] : x[k]);
    }
    const int sh = mp3_headroom_of(mag, 25);
    int16_t __attribute__((aligned(16))) eh[16], el[16], oh[16], ol[16];
    int32_t e0 = 0;
    for (int k = 0; k < 16; k++) {
        const int32_t a = x[k] * (1 << sh), b = x[31 - k] * (1 << sh);
        e0 += a + b;
        mp3_split(eh + k, el + k, a + b);
        mp3_split(oh + k, ol + k, a - b);
    }
    int32_t even[16], odd[16];
    mp3_k_dot16(even + 1, eh, el, mp3_dct16e_split, 15);
    mp3_k_dot16(odd, oh, ol, mp3_dct16_split, 16);

    int16_t __attribute__((aligned(16))) xh[32], xl[32];
    const int32_t round = 1 << sh >> 1;
    for (int r = 0; r < 16; r++) {
        mp3_split(xh + 2 * r, xl + 2 * r, to_v(((r ? 2 * even[r] : e0) + round) >> sh));
        mp3_split(xh + 2 * r + 1, xl + 2 * r + 1, to_v((2 * odd[r] + round) >> sh));
    }

    s->off = (s->off - 64) & 1023;
    int16_t *vh = s->vh + s->off, *vl = s->vl + s->off;
    memcpy(vh, xh + 16, 16 * sizeof(int16_t));
    memcpy(vl, xl + 16, 16 * sizeof(int16_t));
    vh[16] = vl[16] = 0;
    for (int i = 17; i < 48; i++) {
        vh[i] = xh[48 - i];
        vl[i] = xl[48 - i];
    }
    memcpy(vh + 48, xh, 16 * sizeof(int16_t));
    memcpy(vl + 48, xl, 16 * sizeof(int16_t));
    memcpy(vh + 1024, vh, 64 * sizeof(int16_t));
    memcpy(vl + 1024, vl, 64 * sizeof(int16_t));

    int32_t __attribute__((aligned(16))) acc[32];
    mp3_k_window(acc, vh, vl, mp3_window_split);
    for (int j = 0; j < 32; j++) {
        const int32_t y = (acc[j] + SYNTH_FLOOR_BIAS + (1 << (SYNTH_V_FRAC - 16))) >> (SYNTH_V_FRAC - 15);
        out[j * stride] = (int16_t)(y > 32767 ? 32767 : y < -32768 ? -32768 : y);
    }
}

void mp3_emit(mp3_dec_t *d, int blocks, int16_t *pcm) {
    const int in = d->f.h.channels, out = d->base.channels;
    for (int t = 0; t < blocks; t++) {
        if (in == out) {
            for (int ch = 0; ch < out; ch++) synth(&d->synth[ch], d->sb[ch][t], pcm + ch, out);
        } else if (out == 2) {
            synth(&d->synth[0], d->sb[0][t], pcm, 2);
            for (int j = 0; j < 32; j++) pcm[2 * j + 1] = pcm[2 * j];
        } else {
            int32_t mix[32];
            for (int k = 0; k < 32; k++) {
                mix[k] = (int32_t)(((int64_t)d->sb[0][t][k] + d->sb[1][t][k]) >> 1);
            }
            synth(&d->synth[0], mix, pcm, 1);
        }
        pcm += 32 * out;
    }
}

static esp_err_t decode(audf_decoder_t *dec, const void *frame, size_t len, void *pcm, size_t *frames) {
    mp3_dec_t *d = (mp3_dec_t *)dec;
    const uint8_t *p = frame;
    if (len < AUDF_MP3_HEADER_BYTES) return ESP_ERR_INVALID_SIZE;
    if (!parse(p, &d->f)) {
        const bool layer1 = p[0] == 0xff && (p[1] & 0xe6) == 0xe6;
        return layer1 ? ESP_ERR_NOT_SUPPORTED : ESP_ERR_INVALID_RESPONSE;
    }
    if (len < d->f.h.bytes) return ESP_ERR_INVALID_SIZE;
    mp3_k_prepare();
    memcpy(d->frame, p, d->f.h.bytes);
    memset(d->frame + d->f.h.bytes, 0, MP3_PAD);

    const esp_err_t err = d->f.h.layer == 2 ? mp3_l2_decode(d, pcm) : mp3_l3_decode(d, pcm);
    if (err != ESP_OK) return err;
    d->rate = d->f.h.rate;
    *frames = d->f.h.frames;
    return ESP_OK;
}

static void reset(audf_decoder_t *dec) {
    mp3_dec_t *d = (mp3_dec_t *)dec;
    d->reservoir = 0;
    memset(d->overlap, 0, sizeof(d->overlap));
    memset(d->synth, 0, sizeof(d->synth));
}

static void destroy(audf_decoder_t *dec) {
    audf_free(dec);
}

static const audf_decoder_ops_t s_ops = {
    .decode = decode,
    .reset = reset,
    .destroy = destroy,
};

esp_err_t audf_mp3_decoder_create(const audf_mp3_config_t *config, audf_decoder_t **out) {
    if (!config || !out || config->channels < 1 || config->channels > 2) return ESP_ERR_INVALID_ARG;
    mp3_dec_t *d = audf_malloc_aligned(16, sizeof(*d), config->alloc_caps);
    if (!d) return ESP_ERR_NO_MEM;
    memset(d, 0, sizeof(*d));
    d->base.ops = &s_ops;
    d->base.fmt = AUDF_FMT_S16;
    d->base.channels = config->channels;
    d->base.max_frames = AUDF_MP3_MAX_FRAMES;
    *out = &d->base;
    return ESP_OK;
}

uint32_t audf_mp3_decoder_rate(const audf_decoder_t *dec) {
    return ((const mp3_dec_t *)dec)->rate;
}
