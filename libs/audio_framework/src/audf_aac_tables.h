/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdint.h>

typedef struct {
    const uint16_t *lut;
    uint8_t root_bits;
    int8_t  offset;
} aac_huff_t;

enum {
    AAC_HCB1 = 0,
    AAC_HCB2 = 1,
    AAC_HCB3 = 2,
    AAC_HCB4 = 3,
    AAC_HCB5 = 4,
    AAC_HCB6 = 5,
    AAC_HCB7 = 6,
    AAC_HCB8 = 7,
    AAC_HCB9 = 8,
    AAC_HCB10 = 9,
    AAC_HCB11 = 10,
    AAC_HCB_SF = 11,
    SBR_H_T_ENV_1_5 = 12,
    SBR_H_F_ENV_1_5 = 13,
    SBR_H_T_ENV_BAL_1_5 = 14,
    SBR_H_F_ENV_BAL_1_5 = 15,
    SBR_H_T_ENV_3_0 = 16,
    SBR_H_F_ENV_3_0 = 17,
    SBR_H_T_ENV_BAL_3_0 = 18,
    SBR_H_F_ENV_BAL_3_0 = 19,
    SBR_H_T_NOISE_3_0 = 20,
    SBR_H_T_NOISE_BAL_3_0 = 21,
    PS_H_IID_DF1 = 22,
    PS_H_IID_DT1 = 23,
    PS_H_IID_DF0 = 24,
    PS_H_IID_DT0 = 25,
    PS_H_ICC_DF = 26,
    PS_H_ICC_DT = 27,
    PS_H_IPD_DF = 28,
    PS_H_IPD_DT = 29,
    PS_H_OPD_DF = 30,
    PS_H_OPD_DT = 31,
    AAC_HUFF_COUNT = 32,
};

extern const aac_huff_t aac_huff_tables[AAC_HUFF_COUNT];
extern const uint16_t *const aac_swb_offset_long[13];
extern const uint16_t *const aac_swb_offset_short[13];
extern const uint8_t aac_num_swb_long[13];
extern const uint8_t aac_num_swb_short[13];
extern const uint8_t aac_tns_max_bands_long[13];
extern const uint8_t aac_tns_max_bands_short[13];
extern const int32_t aac_pow43_q13[16];
extern const float aac_tns_coef[4][16];
extern const int16_t aac_win_sine_long[1024];
extern const int16_t aac_win_sine_short[128];
extern const int16_t aac_win_kbd_long[1024];
extern const int16_t aac_win_kbd_short[128];
extern const int8_t sbr_start_offset[6][16];
extern const int16_t sbr_qmf_c2r[320];
extern const int16_t sbr_qmf_c[640];
extern const float sbr_noise_table[512][2];
extern const float ps_pd_smooth[512][2];
extern const float ps_ha[46][8][4];
extern const float ps_hb[46][8][4];
extern const float ps_f20[8][7][2];
extern const int8_t ps_k_to_i[71];
extern const uint16_t aac_fft_pos_64[64];
extern const int32_t aac_fft_twiddle31[96];
extern const int32_t aac_dct4_pre31_128[128];
extern const int32_t aac_dct4_post31_128[128];
extern const int16_t aac_q15_dct64_pre[64];
extern const int16_t aac_q15_dct64_post[64];
extern const int16_t aac_q15_fft32[32];
extern const int16_t aac_pie_dct64_pre[256];
extern const int16_t aac_pie_dct64_post[256];
extern const int16_t aac_pie_fft32[128];
extern const int16_t aac_pie_dct1024_pre[1024];
extern const int16_t aac_pie_dct1024_fft128[512];
extern const int16_t aac_pie_dct1024_mid[768];
extern const int16_t aac_pie_dct1024_post[1024];
extern const int16_t aac_pie_ps_ap[448];
