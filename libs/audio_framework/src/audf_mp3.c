/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <string.h>

#include "audf_alloc.h"
#include "audf_mp3_internal.h"

#define SYNTH_FRAC   23
#define SYNTH_IN_MAX ((1 << 26) - 1)
#define SYNTH_V_MAX  (1 << 28)
#define SYNTH_V_UP   2
#define SYNTH_OUT    (SYNTH_FRAC + SYNTH_V_UP - 2)

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

/* tmp holds 3 * (n - 1) values. */
static void dct2(const int32_t *x, int32_t *out, int n, int32_t *tmp) {
    static const int32_t *const s_dct4[17] = {
        [1] = mp3_dct4_1, [2] = mp3_dct4_2, [4] = mp3_dct4_4, [8] = mp3_dct4_8, [16] = mp3_dct4_16,
    };
    if (n < 2) {
        out[0] = x[0];
        return;
    }
    const int h = n >> 1;
    int32_t *e = tmp, *o = tmp + h, *eo = tmp + 2 * h;
    for (int k = 0; k < h; k++) {
        e[k] = x[k] + x[n - 1 - k];
        o[k] = x[k] - x[n - 1 - k];
    }
    dct2(e, eo, h, tmp + 3 * h);
    const int32_t *c = s_dct4[h];
    for (int r = 0; r < h; r++) {
        int32_t acc = 0;
#pragma GCC unroll 16
        for (int k = 0; k < h; k++) acc += mp3_mulh(o[k], c[r * h + k]);
        out[2 * r] = eo[r];
        out[2 * r + 1] = mp3_sat((int64_t)acc * 2);
    }
}

static void synth(mp3_synth_t *s, const int32_t *in, int16_t *out, int stride) {
    int32_t x[32], X[32], tmp[93];
    uint32_t mag = 0;
    for (int k = 0; k < 32; k++) {
        const int32_t v = (in[k] + (1 << (MP3_FRAC - SYNTH_FRAC - 1))) >> (MP3_FRAC - SYNTH_FRAC);
        x[k] = v > SYNTH_IN_MAX ? SYNTH_IN_MAX : v < -SYNTH_IN_MAX ? -SYNTH_IN_MAX : v;
        mag |= (uint32_t)(x[k] < 0 ? -x[k] : x[k]);
    }
    const int sh = mp3_headroom_of(mag, 26);
    if (sh) {
        for (int k = 0; k < 32; k++) x[k] *= 1 << sh;
    }
    dct2(x, X, 32, tmp);
    const int32_t round = 1 << sh >> 1;
    for (int k = 0; k < 32; k++) {
        const int32_t v = (X[k] + round) >> sh;
        X[k] = (v > SYNTH_V_MAX ? SYNTH_V_MAX : v < -SYNTH_V_MAX ? -SYNTH_V_MAX : v) * (1 << SYNTH_V_UP);
    }
    s->off = (s->off - 64) & 1023;
    int32_t *v = s->v + s->off;
    for (int i = 0; i < 16; i++) v[i] = v[i + 1024] = X[16 + i];
    v[16] = v[16 + 1024] = 0;
    for (int i = 17; i < 48; i++) v[i] = v[i + 1024] = -X[48 - i];
    for (int i = 48; i < 64; i++) v[i] = v[i + 1024] = -X[i - 48];

    const int32_t *D = mp3_synth_window;
    for (int j = 0; j < 32; j++) {
        int32_t acc = 8;
#pragma GCC unroll 8
        for (int i = 0; i < 8; i++) {
            acc += mp3_mulh(D[64 * i + j], v[128 * i + j]);
            acc += mp3_mulh(D[64 * i + 32 + j], v[128 * i + 96 + j]);
        }
        const int32_t y = (acc + (1 << (SYNTH_OUT - 16))) >> (SYNTH_OUT - 15);
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
    mp3_dec_t *d = audf_calloc(1, sizeof(*d), config->alloc_caps);
    if (!d) return ESP_ERR_NO_MEM;
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
