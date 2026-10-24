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

extern const int32_t mp3_pow43_q17[1027];
extern const int32_t mp3_pow2_quarter_q27[4];
extern const int32_t mp3_pow2_neg_quarter_q30[4];
extern const int32_t mp3_imdct6[36];
extern const int32_t mp3_imdct_window[4][36];
extern const int32_t mp3_alias_cs[8];
extern const int32_t mp3_alias_ca[8];
extern const int32_t mp3_is_ratio_q30[14];
/* Coefficient rows for the dot kernels, each row as [hi x n][lo x n] of the
 * Q31 values split by mp3_split(). */
extern const int16_t mp3_dct16_split[512];
extern const int16_t mp3_dct16e_split[480];
extern const int16_t mp3_imdct18_split[864];
/* The synthesis window as 22 rows of 16-bit parts (taps over 32767 take two
 * or three), grouped by 8 outputs: [group][row][8]; row p reads V at
 * mp3_window_off[p]. The signs of V are folded in, so V is stored without
 * them. */
extern const int16_t mp3_window_split[704];
extern const uint16_t mp3_window_off[22];
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
