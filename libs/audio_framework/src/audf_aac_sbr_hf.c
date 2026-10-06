/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <float.h>
#include <math.h>
#include <string.h>

#include "audf_aac_sbr.h"

struct sbr_hf {
    float coef[SBR_MAX_BANDS][4];
    int8_t src[SBR_MAX_BANDS];
    float e_orig[SBR_MAX_ENV][SBR_MAX_BANDS];
    float q_mapped[SBR_MAX_ENV][SBR_MAX_BANDS];
    uint8_t s_mapped[SBR_MAX_ENV][SBR_MAX_BANDS];
    uint8_t s_index[SBR_MAX_ENV + 1][SBR_MAX_BANDS];
    float e_curr[SBR_MAX_ENV][SBR_MAX_BANDS];
    float gain[SBR_MAX_ENV][SBR_MAX_BANDS];
    float q_m[SBR_MAX_ENV][SBR_MAX_BANDS];
    float s_m[SBR_MAX_ENV][SBR_MAX_BANDS];
    float g_steady[SBR_MAX_ENV][SBR_MAX_BANDS];
    float q_steady[SBR_MAX_ENV][SBR_MAX_BANDS];
    float alpha[32][4];
    int8_t env_of_slot[2 * 19];
};

size_t sbr_hf_size(void) {
    return sizeof(sbr_hf_t);
}

static void lpc(const aac_sbr_t *s, const int32_t *x_low, sbr_hf_t *w) {
    for (int k = 0; k < s->k0; k++) {
        float re[SBR_XLOW_SLOTS], im[SBR_XLOW_SLOTS];
        for (int n = 0; n < SBR_XLOW_SLOTS; n++) {
            re[n] = (float)x_low[n * 64 + 2 * k];
            im[n] = (float)x_low[n * 64 + 2 * k + 1];
        }
        float r01r = 0, r01i = 0, r02r = 0, r02i = 0, r11 = 0, r12r = 0, r12i = 0, r22 = 0;
        for (int n = 1; n <= 37; n++) {
            r11 += re[n] * re[n] + im[n] * im[n];
            r12r += re[n + 1] * re[n] + im[n + 1] * im[n];
            r12i += im[n + 1] * re[n] - re[n + 1] * im[n];
        }
        r22 = r11 + (re[0] * re[0] + im[0] * im[0]);
        r11 = r11 + (re[38] * re[38] + im[38] * im[38]);
        r01r = r12r + (re[39] * re[38] + im[39] * im[38]);
        r01i = r12i + (im[39] * re[38] - re[39] * im[38]);
        r12r = r12r + (re[1] * re[0] + im[1] * im[0]);
        r12i = r12i + (im[1] * re[0] - re[1] * im[0]);
        for (int n = 0; n < 38; n++) {
            r02r += re[n + 2] * re[n] + im[n + 2] * im[n];
            r02i += im[n + 2] * re[n] - re[n + 2] * im[n];
        }
        float a1r = 0, a1i = 0, a0r = 0, a0i = 0;
        const float dk = r22 * r11 - (r12r * r12r + r12i * r12i) / 1.000001f;
        if (dk != 0.0f) {
            a1r = (r01r * r12r - r01i * r12i - r02r * r11) / dk;
            a1i = (r01r * r12i + r01i * r12r - r02i * r11) / dk;
        }
        if (r11 != 0.0f) {
            a0r = -(r01r + a1r * r12r + a1i * r12i) / r11;
            a0i = -(r01i + a1i * r12r - a1r * r12i) / r11;
        }
        if (a1r * a1r + a1i * a1i >= 16.0f || a0r * a0r + a0i * a0i >= 16.0f) {
            a1r = a1i = a0r = a0i = 0.0f;
        }
        w->alpha[k][0] = a0r;
        w->alpha[k][1] = a0i;
        w->alpha[k][2] = a1r;
        w->alpha[k][3] = a1i;
    }
}

static void chirp(const aac_sbr_t *s, sbr_ch_t *c) {
    static const float tab[4] = { 0.0f, 0.75f, 0.9f, 0.98f };
    for (int i = 0; i < s->n_q; i++) {
        float bw = c->invf[0][i] + c->invf[1][i] == 1 ? 0.6f : tab[c->invf[0][i]];
        if (bw < c->bw[i]) {
            bw = 0.75f * bw + 0.25f * c->bw[i];
        } else {
            bw = 0.90625f * bw + 0.09375f * c->bw[i];
        }
        c->bw[i] = bw < 0.015625f ? 0.0f : bw;
    }
}

static void patch_coefficients(const aac_sbr_t *s, const sbr_ch_t *c, sbr_hf_t *w) {
    memset(w->src, -1, sizeof(w->src));
    int k = s->kx;
    int g = 0;
    for (int j = 0; j < s->num_patches; j++) {
        for (int x = 0; x < s->patch_num[j]; x++, k++) {
            const int p = s->patch_start[j] + x;
            while (g <= s->n_q && k >= s->f_noise[g]) g++;
            g--;
            if (g < 0) g = 0;
            const float bw = c->bw[g];
            float *co = w->coef[k - s->kx];
            co[0] = w->alpha[p][0] * bw;
            co[1] = w->alpha[p][1] * bw;
            co[2] = w->alpha[p][2] * bw * bw;
            co[3] = w->alpha[p][3] * bw * bw;
            w->src[k - s->kx] = (int8_t)p;
        }
    }
}

static inline void x_high(const sbr_hf_t *w, const int32_t *x_low, int m, int idx, float *re, float *im) {
    const int p = w->src[m];
    if (p < 0) {
        *re = 0.0f;
        *im = 0.0f;
        return;
    }
    const float *co = w->coef[m];
    const int32_t *x0 = x_low + idx * 64 + 2 * p;
    const float r0 = (float)x0[0], i0 = (float)x0[1];
    const float r1 = (float)x0[-64], i1 = (float)x0[-63];
    const float r2 = (float)x0[-128], i2 = (float)x0[-127];
    *re = r2 * co[2] - i2 * co[3] + r1 * co[0] - i1 * co[1] + r0;
    *im = i2 * co[2] + r2 * co[3] + i1 * co[0] + r1 * co[1] + i0;
}

static void mapping(const aac_sbr_t *s, sbr_ch_t *c, sbr_hf_t *w) {
    memset(w->s_index, 0, sizeof(w->s_index));
    memcpy(w->s_index[0], c->s_index_prev, sizeof(c->s_index_prev));
    for (int e = 0; e < c->num_env; e++) {
        const int fr = c->freq_res[e + 1];
        const uint8_t *table = fr ? s->f_high : s->f_low;
        const int n = fr ? s->n_high : s->n_low;
        for (int i = 0; i < n; i++) {
            for (int m = table[i]; m < table[i + 1]; m++) w->e_orig[e][m - s->kx] = c->env[e][i];
        }
        const int q = c->num_noise > 1 && c->t_env[e] >= c->t_q[1];
        for (int i = 0; i < s->n_q; i++) {
            for (int m = s->f_noise[i]; m < s->f_noise[i + 1]; m++) w->q_mapped[e][m - s->kx] = c->noise[q][i];
        }
        if (c->add_harmonic_flag) {
            for (int i = 0; i < s->n_high; i++) {
                const int mid = (s->f_high[i] + s->f_high[i + 1]) >> 1;
                w->s_index[e + 1][mid - s->kx] =
                    c->add_harmonic[i] && (e >= c->l_a[1] || w->s_index[0][mid - s->kx] == 1);
            }
        }
        for (int i = 0; i < n; i++) {
            uint8_t present = 0;
            for (int m = table[i]; m < table[i + 1]; m++) present |= w->s_index[e + 1][m - s->kx];
            memset(&w->s_mapped[e][table[i] - s->kx], present, table[i + 1] - table[i]);
        }
    }
    memcpy(c->s_index_prev, w->s_index[c->num_env], sizeof(c->s_index_prev));
}

static void estimate(const aac_sbr_t *s, const sbr_ch_t *c, sbr_hf_t *w) {
    for (int e = 0; e < c->num_env; e++) {
        const int ilb = 2 * c->t_env[e] + SBR_HF_ADJ;
        const int iub = 2 * c->t_env[e + 1] + SBR_HF_ADJ;
        if (s->interpol_freq) {
            const float recip = 1.0f / (float)(iub - ilb);
            for (int m = 0; m < s->m; m++) {
                float sum = 0.0f;
                for (int i = ilb; i < iub; i++) {
                    float re, im;
                    x_high(w, c->x_low, m, i, &re, &im);
                    sum += re * re + im * im;
                }
                w->e_curr[e][m] = sum * recip;
            }
        } else {
            const int fr = c->freq_res[e + 1];
            const uint8_t *table = fr ? s->f_high : s->f_low;
            const int n = fr ? s->n_high : s->n_low;
            for (int p = 0; p < n; p++) {
                float sum = 0.0f;
                for (int k = table[p]; k < table[p + 1]; k++) {
                    for (int i = ilb; i < iub; i++) {
                        float re, im;
                        x_high(w, c->x_low, k - s->kx, i, &re, &im);
                        sum += re * re + im * im;
                    }
                }
                sum /= (float)((iub - ilb) * (table[p + 1] - table[p]));
                for (int k = table[p]; k < table[p + 1]; k++) w->e_curr[e][k - s->kx] = sum;
            }
        }
    }
}

static void gains(const aac_sbr_t *s, const sbr_ch_t *c, sbr_hf_t *w) {
    static const float limgain[4] = { 0.70795f, 1.0f, 1.41254f, 10000000000.0f };
    for (int e = 0; e < c->num_env; e++) {
        const bool delta = !(e == c->l_a[1] || e == c->l_a[0]);
        for (int k = 0; k < s->n_lim; k++) {
            const int m0 = s->f_lim[k] - s->kx, m1 = s->f_lim[k + 1] - s->kx;
            for (int m = m0; m < m1; m++) {
                const float temp = w->e_orig[e][m] / (1.0f + w->q_mapped[e][m]);
                w->q_m[e][m] = sqrtf(temp * w->q_mapped[e][m]);
                w->s_m[e][m] = sqrtf(temp * w->s_index[e + 1][m]);
                if (!w->s_mapped[e][m]) {
                    w->gain[e][m] = sqrtf(w->e_orig[e][m] / ((1.0f + w->e_curr[e][m]) *
                                                             (1.0f + w->q_mapped[e][m] * delta)));
                } else {
                    w->gain[e][m] = sqrtf(w->e_orig[e][m] * w->q_mapped[e][m] /
                                          ((1.0f + w->e_curr[e][m]) * (1.0f + w->q_mapped[e][m])));
                }
                w->gain[e][m] += FLT_MIN;
            }
            float sum0 = 0.0f, sum1 = 0.0f;
            for (int m = m0; m < m1; m++) {
                sum0 += w->e_orig[e][m];
                sum1 += w->e_curr[e][m];
            }
            float gain_max = limgain[s->limiter_gains] * sqrtf((FLT_EPSILON + sum0) / (FLT_EPSILON + sum1));
            if (gain_max > 100000.0f) gain_max = 100000.0f;
            for (int m = m0; m < m1; m++) {
                const float q_max = w->q_m[e][m] * gain_max / w->gain[e][m];
                if (w->q_m[e][m] > q_max) w->q_m[e][m] = q_max;
                if (w->gain[e][m] > gain_max) w->gain[e][m] = gain_max;
            }
            sum0 = sum1 = 0.0f;
            for (int m = m0; m < m1; m++) {
                sum0 += w->e_orig[e][m];
                sum1 += w->e_curr[e][m] * w->gain[e][m] * w->gain[e][m] + w->s_m[e][m] * w->s_m[e][m] +
                        (delta && !w->s_m[e][m]) * w->q_m[e][m] * w->q_m[e][m];
            }
            float boost = sqrtf((FLT_EPSILON + sum0) / (FLT_EPSILON + sum1));
            if (boost > 1.584893192f) boost = 1.584893192f;
            for (int m = m0; m < m1; m++) {
                w->gain[e][m] *= boost;
                w->q_m[e][m] *= boost;
                w->s_m[e][m] *= boost;
            }
        }
    }
}

static inline int32_t to_fixed(float v) {
    if (v > 1073741824.0f) return 1073741824;
    if (v < -1073741824.0f) return -1073741824;
    return (int32_t)lrintf(v);
}

static const float s_h_smooth[5] = {
    0.33333333333333f, 0.30150283239582f, 0.21816949906249f, 0.11516383427084f, 0.03183050093751f,
};

static const float *gain_row(const aac_sbr_t *s, const sbr_ch_t *c, const sbr_hf_t *w, int slot, bool q) {
    const int t0 = 2 * c->t_env[0];
    if (slot >= t0) {
        const int e = w->env_of_slot[slot];
        return q ? w->q_m[e] : w->gain[e];
    }
    if (s->reset) return q ? w->q_m[0] : w->gain[0];
    return q ? c->q_hist[slot - (t0 - 4)] : c->g_hist[slot - (t0 - 4)];
}

void sbr_hf_prepare(aac_sbr_t *s, sbr_ch_t *c, sbr_hf_t *w) {
    lpc(s, c->x_low, w);
    chirp(s, c);
    patch_coefficients(s, c, w);
    mapping(s, c, w);
    estimate(s, c, w);
    gains(s, c, w);
    for (int e = 0; e < c->num_env; e++) {
        for (int i = 2 * c->t_env[e]; i < 2 * c->t_env[e + 1]; i++) w->env_of_slot[i] = (int8_t)e;
        for (int m = 0; m < s->m; m++) {
            float gs = 0.0f, qs = 0.0f;
            for (int j = 0; j <= 4; j++) {
                gs += w->gain[e][m] * s_h_smooth[j];
                qs += w->q_m[e][m] * s_h_smooth[j];
            }
            w->g_steady[e][m] = gs;
            w->q_steady[e][m] = qs;
        }
    }
}

void sbr_hf_slot(aac_sbr_t *s, sbr_ch_t *c, const sbr_hf_t *w, int i, int32_t *row) {
    static const int8_t phi_re[4] = { 1, 0, -1, 0 };
    static const int8_t phi_im[4] = { 0, 1, 0, -1 };
    const int h_sl = s->smoothing_mode ? 0 : 4;
    const int kx = s->kx;
    const int m_max = s->m;
    const int e = w->env_of_slot[i];
    const bool transient = e == c->l_a[0] || e == c->l_a[1];
    float g_filt_buf[SBR_MAX_BANDS], q_filt_buf[SBR_MAX_BANDS];
    const float *g_filt = g_filt_buf, *q_filt = q_filt_buf;
    if (h_sl && !transient && i - h_sl >= 2 * c->t_env[e]) {
        g_filt = w->g_steady[e];
        q_filt = w->q_steady[e];
    } else if (h_sl && !transient) {
        const float *g[5], *q[5];
        for (int j = 0; j <= h_sl; j++) {
            g[j] = gain_row(s, c, w, i - j, false);
            q[j] = gain_row(s, c, w, i - j, true);
        }
        for (int m = 0; m < m_max; m++) {
            float gs = 0.0f, qs = 0.0f;
            for (int j = 0; j <= h_sl; j++) {
                gs += g[j][m] * s_h_smooth[j];
                qs += q[j][m] * s_h_smooth[j];
            }
            g_filt_buf[m] = gs;
            q_filt_buf[m] = qs;
        }
    } else {
        g_filt = w->gain[e];
        q_filt = w->q_m[e];
    }
    int noise = c->noise_index;
    const int sine = c->sine_index;
    for (int m = 0; m < m_max; m++) {
        float xr, xi;
        x_high(w, c->x_low, m, i + SBR_HF_ADJ, &xr, &xi);
        float re = xr * g_filt[m];
        float im = xi * g_filt[m];
        noise = (noise + 1) & 0x1ff;
        const float sm = w->s_m[e][m];
        if (sm != 0.0f) {
            re += sm * phi_re[sine];
            im += ((kx + m) & 1) ? -sm * phi_im[sine] : sm * phi_im[sine];
        } else if (!transient) {
            re += q_filt[m] * sbr_noise_table[noise][0];
            im += q_filt[m] * sbr_noise_table[noise][1];
        }
        if (row) {
            row[2 * (kx + m)] = to_fixed(re);
            row[2 * (kx + m) + 1] = to_fixed(im);
        }
    }
    c->noise_index = (uint16_t)noise;
    c->sine_index = (sine + 1) & 3;
}

void sbr_hf_finish(aac_sbr_t *s, sbr_ch_t *c, const sbr_hf_t *w) {
    if (s->smoothing_mode) return;
    const int end = 2 * c->t_env[c->num_env];
    float g[4][SBR_MAX_BANDS], q[4][SBR_MAX_BANDS];
    for (int i = 0; i < 4; i++) {
        memcpy(g[i], gain_row(s, c, w, end - 4 + i, false), sizeof(g[i]));
        memcpy(q[i], gain_row(s, c, w, end - 4 + i, true), sizeof(q[i]));
    }
    memcpy(c->g_hist, g, sizeof(g));
    memcpy(c->q_hist, q, sizeof(q));
}
