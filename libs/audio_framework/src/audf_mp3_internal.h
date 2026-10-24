/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdbool.h>
#include <stdint.h>

#include "audf_codec_internal.h"
#include "audf_mp3.h"
#include "audf_mp3_tables.h"

/* Spectral and subband samples are PCM x 2^MP3_FRAC. */
#define MP3_FRAC      24
#define MP3_MAX_FRAME 1732
#define MP3_MAX_BACK  511
#define MP3_PAD       16

enum {
    MP3_STEREO,
    MP3_JOINT,
    MP3_DUAL,
    MP3_MONO,
};

typedef struct {
    audf_mp3_header_t h;
    uint16_t kbps;
    uint8_t  version;
    uint8_t  rate_index;
    uint8_t  mode;
    uint8_t  mode_ext;
    bool     crc;
} mp3_frame_t;

typedef struct {
    const uint8_t *buf;
    uint32_t pos;
    uint32_t end;
} mp3_bits_t;

typedef struct {
    int16_t __attribute__((aligned(16))) vh[2048];
    int16_t __attribute__((aligned(16))) vl[2048];
    unsigned off;
} mp3_synth_t;

typedef struct {
    audf_decoder_t base;
    mp3_frame_t f;
    uint32_t rate;
    uint32_t reservoir;
    uint8_t  frame[MP3_MAX_FRAME + MP3_PAD];
    uint8_t  main[MP3_MAX_BACK + MP3_MAX_FRAME + MP3_PAD];
    uint8_t  scf_prev[2][22];
    int32_t  xr[2][576];
    int32_t  scratch[576];
    int32_t  overlap[2][576];
    int32_t  sb[2][18][32];
    mp3_synth_t synth[2];
} mp3_dec_t;

/* Reads past end return zeros; buf needs MP3_PAD readable bytes after end. */
static inline uint32_t mp3_bits_show(const mp3_bits_t *b) {
    if (b->pos >= b->end + 32) return 0;
    const uint8_t *p = b->buf + (b->pos >> 3);
    uint32_t v = (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 | p[3];
    const unsigned s = b->pos & 7;
    if (s) v = v << s | p[4] >> (8 - s);
    return v;
}

static inline uint32_t mp3_bits_get(mp3_bits_t *b, unsigned n) {
    if (!n) return 0;
    const uint32_t v = mp3_bits_show(b) >> (32 - n);
    b->pos += n;
    return v;
}

static inline int32_t mp3_mulh(int32_t a, int32_t b) {
    return (int32_t)(((int64_t)a * b) >> 32);
}

/* Left shift that brings `mag` (an OR of magnitudes) just under 2^bits. */
static inline int mp3_headroom_of(uint32_t mag, int bits) {
    const int s = mag ? bits - (32 - __builtin_clz(mag)) : 0;
    return s > 0 ? s : 0;
}

static inline int mp3_headroom(const int32_t *x, int n, int bits) {
    uint32_t mag = 0;
    for (int i = 0; i < n; i++) mag |= (uint32_t)(x[i] < 0 ? -x[i] : x[i]);
    return mp3_headroom_of(mag, bits);
}

/* Synthesizes `blocks` rows of d->sb into interleaved PCM. */
void      mp3_emit(mp3_dec_t *d, int blocks, int16_t *pcm);
esp_err_t mp3_l2_decode(mp3_dec_t *d, int16_t *pcm);
esp_err_t mp3_l3_decode(mp3_dec_t *d, int16_t *pcm);
