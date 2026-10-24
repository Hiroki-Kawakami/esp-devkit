/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdint.h>

extern const uint8_t silk_lbrr_icdf[2][7];
extern const uint8_t silk_stereo_s1_icdf[25];
extern const uint8_t silk_stereo_s2_icdf[3];
extern const uint8_t silk_stereo_s3_icdf[5];
extern const int16_t silk_stereo_weights[16];
extern const uint8_t silk_mid_only_icdf[2];
extern const uint8_t silk_type_inactive_icdf[2];
extern const uint8_t silk_type_active_icdf[4];
extern const uint8_t silk_gain_msb_icdf[3][8];
extern const uint8_t silk_uniform8_icdf[8];
extern const uint8_t silk_delta_gain_icdf[41];
extern const uint8_t silk_lsf_s1_icdf[4][32];
extern const uint8_t silk_lsf_s2_icdf[16][9];
extern const uint8_t silk_lsf_sel_nbmb[32][10];
extern const uint8_t silk_lsf_sel_wb[32][16];
extern const uint8_t silk_lsf_ext_icdf[7];
extern const uint8_t silk_lsf_pred_nbmb[2][9];
extern const uint8_t silk_lsf_pred_wb[2][15];
extern const uint8_t silk_lsf_pred_sel_nbmb[32][9];
extern const uint8_t silk_lsf_pred_sel_wb[32][15];
extern const uint8_t silk_lsf_cb_nbmb[32][10];
extern const uint8_t silk_lsf_cb_wb[32][16];
extern const int16_t silk_lsf_min_spacing_nbmb[11];
extern const int16_t silk_lsf_min_spacing_wb[17];
extern const uint8_t silk_lsf_interp_icdf[5];
extern const uint8_t silk_lsf_order_nbmb[10];
extern const uint8_t silk_lsf_order_wb[16];
extern const int16_t silk_lsf_cos_q12[129];
extern const uint8_t silk_pitch_high_icdf[32];
extern const uint8_t silk_pitch_low_nb_icdf[4];
extern const uint8_t silk_pitch_low_mb_icdf[6];
extern const uint8_t silk_pitch_low_wb_icdf[8];
extern const uint8_t silk_pitch_delta_icdf[21];
extern const uint8_t silk_contour_nb10_icdf[3];
extern const uint8_t silk_contour_nb20_icdf[11];
extern const uint8_t silk_contour_wb10_icdf[12];
extern const uint8_t silk_contour_wb20_icdf[34];
extern const int8_t silk_contour_nb10[3][2];
extern const int8_t silk_contour_nb20[11][4];
extern const int8_t silk_contour_wb10[12][2];
extern const int8_t silk_contour_wb20[34][4];
extern const uint8_t silk_ltp_per_icdf[3];
extern const uint8_t silk_ltp_filter0_icdf[8];
extern const uint8_t silk_ltp_filter1_icdf[16];
extern const uint8_t silk_ltp_filter2_icdf[32];
extern const int8_t silk_ltp_taps0[8][5];
extern const int8_t silk_ltp_taps1[16][5];
extern const int8_t silk_ltp_taps2[32][5];
extern const uint8_t silk_ltp_scale_icdf[3];
extern const uint8_t silk_uniform4_icdf[4];
extern const uint8_t silk_rate_level_icdf[2][9];
extern const uint8_t silk_pulse_count_icdf[11][18];
extern const uint8_t silk_shell_icdf[4][16][17];
extern const uint8_t silk_lsb_icdf[2];
extern const uint8_t silk_sign_icdf[42];
/* Q15 coefficients of the two all-pass branches of the 2x upsampler */
extern const int16_t silk_up2_allpass[2][3];
/* Q15 half of the 8-tap interpolation filter for each 1/12 phase */
extern const int16_t silk_frac_fir[12][4];
