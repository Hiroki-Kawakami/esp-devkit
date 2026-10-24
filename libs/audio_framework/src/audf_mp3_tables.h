/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdint.h>

typedef struct {
    uint16_t offset;
    uint8_t  root_bits;
    uint8_t  linbits;
} mp3_huff_t;

typedef struct {
    uint16_t steps;
    uint8_t  bits;
    uint8_t  grouped;
} mp3_l2_class_t;

typedef struct {
    uint8_t subbands;
    uint8_t nbal;
    uint8_t classes[15];
} mp3_l2_alloc_t;

extern const uint16_t mp3_huff_lut[];
extern const mp3_huff_t mp3_huff_pairs[32];
extern const mp3_huff_t mp3_huff_quad;

extern const int32_t mp3_synth_window[512];
extern const int32_t mp3_dct4_16[256];
extern const int32_t mp3_dct4_8[64];
extern const int32_t mp3_dct4_4[16];
extern const int32_t mp3_dct4_2[4];
extern const int32_t mp3_dct4_1[1];

extern const int32_t mp3_pow43_q17[1027];
extern const int32_t mp3_pow2_quarter_q27[4];
extern const int32_t mp3_pow2_neg_quarter_q30[4];
extern const int32_t mp3_imdct18[324];
extern const int32_t mp3_imdct6[36];
extern const int32_t mp3_imdct_window[4][36];
extern const int32_t mp3_alias_cs[8];
extern const int32_t mp3_alias_ca[8];
extern const int32_t mp3_is_ratio_q30[14];
extern const uint8_t mp3_sfb_long[9][22];
extern const uint8_t mp3_sfb_short[9][13];
extern const uint8_t mp3_pretab[22];
extern const uint8_t mp3_slen[16][2];
extern const uint8_t mp3_lsf_nsfb[6][3][4];

extern const int32_t  mp3_l2_scale_q29[64];
extern const uint32_t mp3_l2_inv_steps_q32[17];
extern const mp3_l2_class_t mp3_l2_classes[17];
extern const mp3_l2_alloc_t mp3_l2_alloc_high[4];
extern const mp3_l2_alloc_t mp3_l2_alloc_low[2];
extern const mp3_l2_alloc_t mp3_l2_alloc_lsf[3];
