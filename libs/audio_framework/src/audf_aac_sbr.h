/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include "audf_aac_internal.h"

#define SBR_SLOTS      32
#define SBR_XLOW_SLOTS 40
#define SBR_HF_ADJ     2
#define SBR_HF_GEN     8
#define SBR_MAX_ENV    5
#define SBR_MAX_BANDS  48
#define SBR_MAX_NQ     5
#define SBR_V_LEN      2560
#define SBR_ANA_LEN    1312

typedef struct {
    uint8_t  frame_class;
    uint8_t  num_env;
    uint8_t  num_noise;
    uint8_t  amp_res;
    uint8_t  freq_res[SBR_MAX_ENV + 1];
    uint8_t  t_env[SBR_MAX_ENV + 1];
    uint8_t  t_q[3];
    uint8_t  t_env_prev_end;
    int8_t   l_a[2];
    uint8_t  df_env[SBR_MAX_ENV];
    uint8_t  df_noise[2];
    uint8_t  invf[2][SBR_MAX_NQ];
    int16_t  env_q[SBR_MAX_ENV + 1][SBR_MAX_BANDS];
    int16_t  noise_q[3][SBR_MAX_NQ];
    bool     add_harmonic_flag;
    uint8_t  add_harmonic[SBR_MAX_BANDS];
    uint8_t  s_index_prev[SBR_MAX_BANDS];
    float    env[SBR_MAX_ENV][SBR_MAX_BANDS];
    float    noise[2][SBR_MAX_NQ];
    float    bw[SBR_MAX_NQ];
    float    g_hist[4][SBR_MAX_BANDS];
    float    q_hist[4][SBR_MAX_BANDS];
    uint16_t noise_index;
    uint8_t  sine_index;
    int16_t *ana_hi;
    int16_t *ana_lo;
    int32_t *x_low;
    int32_t *y_tail;
    int16_t *v_hi;
    int16_t *v_lo;
    int      v_off;
} sbr_ch_t;

typedef struct aac_ps aac_ps_t;
typedef struct sbr_hf sbr_hf_t;

struct aac_sbr {
    uint32_t rate;
    bool     start;
    bool     reset;
    bool     have_data;
    int      element;
    bool     coupling;
    uint8_t  amp_res_header;
    int8_t   start_freq, stop_freq, xover, freq_scale, alter_scale, noise_bands;
    uint8_t  limiter_bands, limiter_gains, interpol_freq, smoothing_mode;

    uint8_t  k0, k2;
    uint8_t  kx, m, kx_prev, m_prev;
    uint8_t  n_master, n_high, n_low, n_q, n_lim;
    uint8_t  f_master[64];
    uint8_t  f_high[SBR_MAX_BANDS + 1];
    uint8_t  f_low[SBR_MAX_BANDS + 1];
    uint8_t  f_noise[SBR_MAX_NQ + 1];
    uint8_t  f_lim[SBR_MAX_BANDS + 8];
    uint8_t  num_patches;
    uint8_t  patch_num[6];
    uint8_t  patch_start[6];

    sbr_ch_t ch[2];
    sbr_hf_t *hf[2];
    aac_ps_t *ps;
};

/* Writes the frame's x_low rows up to subband bands. */
void sbr_qmf_analysis(sbr_ch_t *c, const int32_t *time, int bands, int32_t *tmp);
/* Four consecutive slots of 64 complex subbands, row i at x[i], into acc[i * 64];
 * tmp holds 768 words. */
void sbr_qmf_synthesis(sbr_ch_t *c, const int32_t *const x[4], int32_t *acc, int32_t *tmp);

size_t sbr_hf_size(void);
void sbr_hf_prepare(aac_sbr_t *s, sbr_ch_t *c, sbr_hf_t *w);
/* Writes the high band of slot i into row, or only advances the noise and
 * sinusoid phase when row is NULL; slots are visited in order. */
void sbr_hf_slot(aac_sbr_t *s, sbr_ch_t *c, const sbr_hf_t *w, int i, int32_t *row);
void sbr_hf_finish(aac_sbr_t *s, sbr_ch_t *c, const sbr_hf_t *w);
