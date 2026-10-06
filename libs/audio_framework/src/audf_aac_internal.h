/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdbool.h>
#include <stdint.h>

#include "audf_aac.h"
#include "audf_aac_tables.h"
#include "audf_codec_internal.h"

#define AAC_FRAME_LEN    1024
#define AAC_MAX_SFB      51
#define AAC_MAX_BYTES    8192
#define AAC_BUF_PAD      8

enum {
    AAC_ONLY_LONG = 0,
    AAC_LONG_START,
    AAC_EIGHT_SHORT,
    AAC_LONG_STOP,
};

enum {
    AAC_ZERO_HCB = 0,
    AAC_ESC_HCB = 11,
    AAC_NOISE_HCB = 13,
    AAC_INTENSITY_HCB2 = 14,
    AAC_INTENSITY_HCB = 15,
};

enum {
    AAC_ID_SCE = 0,
    AAC_ID_CPE,
    AAC_ID_CCE,
    AAC_ID_LFE,
    AAC_ID_DSE,
    AAC_ID_PCE,
    AAC_ID_FIL,
    AAC_ID_END,
};

typedef struct {
    const uint8_t *buf;
    uint32_t pos;
    uint32_t end;
} aac_bits_t;

/* Reads past end return zeros; buf needs AAC_BUF_PAD readable bytes after end. */
static inline uint32_t aac_bits_show(const aac_bits_t *b) {
    if (b->pos >= b->end + 32) return 0;
    const uint8_t *p = b->buf + (b->pos >> 3);
    uint32_t v = (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 | p[3];
    const unsigned s = b->pos & 7;
    if (s) v = v << s | p[4] >> (8 - s);
    return v;
}

static inline uint32_t aac_bits_get(aac_bits_t *b, unsigned n) {
    if (!n) return 0;
    const uint32_t v = aac_bits_show(b) >> (32 - n);
    b->pos += n;
    return v;
}

static inline bool aac_bits_bit(aac_bits_t *b) {
    return aac_bits_get(b, 1);
}

static inline bool aac_bits_overrun(const aac_bits_t *b) {
    return b->pos > b->end;
}

static inline unsigned aac_bits_left(const aac_bits_t *b) {
    return b->pos < b->end ? b->end - b->pos : 0;
}

static inline int aac_huff(aac_bits_t *b, int table) {
    const aac_huff_t *h = &aac_huff_tables[table];
    const uint32_t w = aac_bits_show(b);
    unsigned bits = h->root_bits;
    unsigned base = 0;
    uint32_t e = h->lut[w >> (32 - bits)];
    while (e >> 15) {
        base += bits;
        bits = (e >> 12) & 7;
        e = h->lut[(e & 0xfff) + ((w << base) >> (32 - bits))];
    }
    b->pos += base + (e >> 9);
    return (int)(e & 0x1ff) + h->offset;
}

typedef struct {
    uint8_t  n_filt;
    uint8_t  coef_res;
    uint8_t  length[3];
    uint8_t  order[3];
    uint8_t  direction[3];
    uint8_t  compress[3];
    uint8_t  coef[3][12];
} aac_tns_win_t;

typedef struct {
    uint8_t  window_sequence;
    uint8_t  window_shape;
    uint8_t  max_sfb;
    uint8_t  num_windows;
    uint8_t  num_groups;
    uint8_t  group_len[8];
    uint8_t  num_swb;
    const uint16_t *swb_offset;
    uint8_t  global_gain;
    uint8_t  cb[8][AAC_MAX_SFB + 1];
    int16_t  sf[8][AAC_MAX_SFB + 1];
    bool     pulse;
    uint8_t  pulse_n;
    uint8_t  pulse_start;
    uint8_t  pulse_offset[4];
    uint8_t  pulse_amp[4];
    bool     tns;
    aac_tns_win_t tns_win[8];
} aac_ics_t;

typedef struct {
    aac_ics_t ics;
    int32_t  *spec;
    int32_t  *overlap;
    int32_t  *time;
    uint8_t   prev_shape;
} aac_chan_t;

typedef struct aac_sbr aac_sbr_t;

typedef struct {
    audf_decoder_t base;
    uint8_t   sf_index;
    uint8_t   core_channels;
    uint32_t  core_rate;
    uint32_t  out_rate;
    bool      adts;
    bool      sbr_on;
    bool      ps_on;
    uint8_t   ms_used[8][AAC_MAX_SFB + 1];
    uint8_t   ms_present;
    bool      common_window;
    aac_chan_t ch[2];
    uint32_t  noise_seed;
    uint32_t  noise_seeds[8][AAC_MAX_SFB + 1];
    uint8_t  *frame;
    int32_t  *scratch;
    aac_sbr_t *sbr;
    uint32_t  alloc_caps;
    uint32_t (*clock)(void);
    uint32_t  prof_mark;
    uint64_t  prof[AUDF_AAC_PROF_COUNT];
} aac_dec_t;

static inline void aac_prof_start(aac_dec_t *d) {
    if (d->clock) d->prof_mark = d->clock();
}

static inline void aac_prof(aac_dec_t *d, int stage) {
    if (!d->clock) return;
    const uint32_t now = d->clock();
    d->prof[stage] += now - d->prof_mark;
    d->prof_mark = now;
}

esp_err_t aac_ics_parse(aac_dec_t *d, aac_bits_t *b, aac_chan_t *c, bool common_window);
esp_err_t aac_ics_info(aac_dec_t *d, aac_bits_t *b, aac_ics_t *ics);
void aac_spectrum(aac_dec_t *d, int channels);
void aac_filterbank(aac_dec_t *d, aac_chan_t *c);

esp_err_t aac_sbr_create(aac_dec_t *d);
void aac_sbr_destroy(aac_dec_t *d);
void aac_sbr_reset(aac_dec_t *d);
esp_err_t aac_sbr_parse(aac_dec_t *d, aac_bits_t *b, int element, unsigned bits, bool crc);
void aac_sbr_frame_start(aac_dec_t *d);
void aac_sbr_apply(aac_dec_t *d, int16_t *out);
