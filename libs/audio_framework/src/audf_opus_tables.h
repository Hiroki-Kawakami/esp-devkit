/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdint.h>

#define CELT_BANDS         21
#define CELT_ALLOC_VECTORS 11
#define CELT_OVERLAP       120

extern const int16_t celt_ebands[CELT_BANDS + 1];
extern const uint8_t celt_alloc[CELT_ALLOC_VECTORS][CELT_BANDS];
extern const uint8_t celt_e_prob[4][2][42];
/* Q4 */
extern const int8_t  celt_e_means[CELT_BANDS];
/* Q15 */
extern const int16_t celt_pred_coef[4];
extern const int16_t celt_beta_coef[4];
extern const int16_t celt_log_n[CELT_BANDS];
extern const int16_t celt_cache_index[105];
extern const uint8_t celt_cache_bits[392];
extern const uint8_t celt_cache_caps[168];
/* Q15 */
extern const int16_t celt_window[CELT_OVERLAP];
extern const uint8_t celt_log2_frac[24];
extern const int8_t  celt_tf_select[4][8];
extern const uint8_t celt_trim_icdf[11];
extern const uint8_t celt_spread_icdf[4];
extern const uint8_t celt_tapset_icdf[3];
extern const uint8_t celt_small_energy_icdf[3];
extern const int16_t celt_exp2_frac8[8];
extern const uint8_t celt_hadamard_order[30];
/* Q15 */
extern const int16_t celt_comb_gains[3][3];
