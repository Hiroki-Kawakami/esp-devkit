/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <math.h>
#include <stdlib.h>
#include <string.h>

#include "audf_aac_kernels.h"
#include "audf_aac_ps.h"
#include "audf_aac_sbr.h"
#include "audf_alloc.h"

enum { FIXFIX = 0, FIXVAR, VARFIX, VARVAR };

static void turn_off(aac_sbr_t *s) {
    s->start = false;
    s->have_data = false;
    s->kx = 32;
    s->m = 0;
    s->ch[0].l_a[1] = s->ch[1].l_a[1] = -1;
    s->start_freq = s->stop_freq = s->xover = -1;
    s->freq_scale = s->alter_scale = s->noise_bands = -1;
}

static int nint(double v) {
    return (int)floor(v + 0.5);
}

static int cmp_u8(const void *a, const void *b) {
    return *(const uint8_t *)a - *(const uint8_t *)b;
}

static void make_bands(uint8_t *dk, int start, int stop, int bands) {
    int prev = start;
    for (int k = 0; k < bands; k++) {
        const int cur = nint(start * pow((double)stop / start, (double)(k + 1) / bands));
        dk[k] = (uint8_t)(cur - prev);
        prev = cur;
    }
}

static bool make_master(aac_sbr_t *s) {
    int row;
    switch (s->rate) {
    case 16000: row = 0; break;
    case 22050: row = 1; break;
    case 24000: row = 2; break;
    case 32000: row = 3; break;
    case 44100: case 48000: case 64000: row = 4; break;
    case 88200: case 96000: row = 5; break;
    default: return false;
    }
    const int temp = s->rate < 32000 ? 3000 : s->rate < 64000 ? 4000 : 5000;
    const int start_min = (int)(((uint32_t)temp * 128 + s->rate / 2) / s->rate);
    const int stop_min = (int)(((uint32_t)temp * 256 + s->rate / 2) / s->rate);
    const int k0 = start_min + sbr_start_offset[row][s->start_freq];
    int k2;
    if (s->stop_freq < 14) {
        uint8_t dk[13];
        make_bands(dk, stop_min, 64, 13);
        qsort(dk, 13, 1, cmp_u8);
        k2 = stop_min;
        for (int k = 0; k < s->stop_freq; k++) k2 += dk[k];
    } else {
        k2 = (s->stop_freq == 14 ? 2 : 3) * k0;
    }
    if (k2 > 64) k2 = 64;
    const int max_bands = s->rate <= 32000 ? 48 : s->rate == 44100 ? 35 : 32;
    if (k2 - k0 > max_bands || k0 >= k2) return false;
    s->k0 = (uint8_t)k0;
    s->k2 = (uint8_t)k2;

    if (!s->freq_scale) {
        const int dk = s->alter_scale + 1;
        const int n = ((k2 - k0 + (dk & 2)) >> dk) << 1;
        if (n <= 0 || n > 63) return false;
        uint8_t v[64];
        for (int k = 0; k < n; k++) v[k] = (uint8_t)dk;
        int diff = k2 - (k0 + n * dk);
        if (diff < 0) {
            for (int k = 0; diff < 0 && k < n; k++, diff++) v[k]--;
        } else {
            for (int k = n - 1; diff > 0 && k >= 0; k--, diff--) v[k]++;
        }
        s->f_master[0] = (uint8_t)k0;
        for (int k = 1; k <= n; k++) s->f_master[k] = s->f_master[k - 1] + v[k - 1];
        s->n_master = (uint8_t)n;
    } else {
        static const int bands_tab[3] = { 12, 10, 8 };
        const int bands = bands_tab[s->freq_scale - 1];
        const bool two = 49 * k2 > 110 * k0;
        const int k1 = two ? 2 * k0 : k2;
        const int n0 = 2 * nint(bands * log2((double)k1 / k0) / 2.0);
        if (n0 <= 0 || n0 > 48) return false;
        uint8_t dk0[48];
        make_bands(dk0, k0, k1, n0);
        qsort(dk0, n0, 1, cmp_u8);
        s->f_master[0] = (uint8_t)k0;
        for (int k = 1; k <= n0; k++) {
            if (!dk0[k - 1]) return false;
            s->f_master[k] = s->f_master[k - 1] + dk0[k - 1];
        }
        int n = n0;
        if (two) {
            const double warp = s->alter_scale ? 1.3 : 1.0;
            const int n1 = 2 * nint(bands * log2((double)k2 / k1) / (2.0 * warp));
            if (n1 <= 0 || n0 + n1 > 63) return false;
            uint8_t dk1[48];
            make_bands(dk1, k1, k2, n1);
            qsort(dk1, n1, 1, cmp_u8);
            if (dk1[0] < dk0[n0 - 1]) {
                int change = dk0[n0 - 1] - dk1[0];
                const int limit = (dk1[n1 - 1] - dk1[0]) >> 1;
                if (change > limit) change = limit;
                dk1[0] += change;
                dk1[n1 - 1] -= change;
                qsort(dk1, n1, 1, cmp_u8);
            }
            for (int k = 1; k <= n1; k++) {
                if (!dk1[k - 1]) return false;
                s->f_master[n0 + k] = s->f_master[n0 + k - 1] + dk1[k - 1];
            }
            n += n1;
        }
        s->n_master = (uint8_t)n;
    }
    return s->xover < s->n_master;
}

static bool in_list(const uint8_t *list, int n, int v) {
    for (int i = 0; i < n; i++) {
        if (list[i] == v) return true;
    }
    return false;
}

static void make_limiter(aac_sbr_t *s) {
    if (!s->limiter_bands) {
        s->f_lim[0] = s->f_low[0];
        s->f_lim[1] = s->f_low[s->n_low];
        s->n_lim = 1;
        return;
    }
    static const double warped[3] = { 1.32715174233856803909, 1.18509277094158210129, 1.11987160404675912501 };
    const double ratio = warped[s->limiter_bands - 1];
    uint8_t borders[7];
    borders[0] = s->kx;
    for (int k = 1; k <= s->num_patches; k++) borders[k] = borders[k - 1] + s->patch_num[k - 1];
    uint8_t t[SBR_MAX_BANDS + 8];
    int n = s->n_low + 1;
    memcpy(t, s->f_low, n);
    for (int k = 1; k < s->num_patches; k++) t[n++] = borders[k];
    qsort(t, n, 1, cmp_u8);
    int k = 1;
    while (k < n) {
        if (t[k] >= t[k - 1] * ratio) {
            k++;
        } else if (t[k] == t[k - 1] || !in_list(borders, s->num_patches + 1, t[k])) {
            memmove(t + k, t + k + 1, n - k - 1);
            n--;
        } else if (!in_list(borders, s->num_patches + 1, t[k - 1])) {
            memmove(t + k - 1, t + k, n - k);
            n--;
        } else {
            k++;
        }
    }
    memcpy(s->f_lim, t, n);
    s->n_lim = (uint8_t)(n - 1);
}

static bool make_patches(aac_sbr_t *s) {
    const int goal = (int)(((1000u << 11) + s->rate / 2) / s->rate);
    int msb = s->k0, usb = s->kx;
    int k, sb = 0;
    if (goal < s->kx + s->m) {
        for (k = 0; s->f_master[k] < goal; k++) {
        }
    } else {
        k = s->n_master;
    }
    s->num_patches = 0;
    int last_k = -1, last_msb = -1;
    do {
        if (k == last_k && msb == last_msb) return false;
        last_k = k;
        last_msb = msb;
        int odd = 0;
        for (int j = k; j == k || sb > s->k0 - 1 + msb - odd; j--) {
            if (j < 0) return false;
            sb = s->f_master[j];
            odd = (sb + s->k0) & 1;
        }
        if (s->num_patches > 5) return false;
        const int num = sb - usb > 0 ? sb - usb : 0;
        s->patch_num[s->num_patches] = (uint8_t)num;
        s->patch_start[s->num_patches] = (uint8_t)(s->k0 - odd - num);
        if (num > 0) {
            usb = sb;
            msb = sb;
            s->num_patches++;
        } else {
            msb = s->kx;
        }
        if (s->f_master[k] - sb < 3) k = s->n_master;
    } while (sb != s->kx + s->m);
    if (s->num_patches > 1 && s->patch_num[s->num_patches - 1] < 3) s->num_patches--;
    for (int j = 0; j < s->num_patches; j++) {
        if (s->patch_start[j] + s->patch_num[j] > 32) return false;
    }
    return s->num_patches > 0;
}

static bool make_tables(aac_sbr_t *s) {
    if (!make_master(s)) return false;
    s->n_high = s->n_master - s->xover;
    s->n_low = (s->n_high + 1) >> 1;
    memcpy(s->f_high, s->f_master + s->xover, s->n_high + 1);
    s->kx = s->f_high[0];
    s->m = s->f_high[s->n_high] - s->kx;
    if (s->kx + s->m > 64 || s->kx > 32 || s->m > SBR_MAX_BANDS) return false;
    s->f_low[0] = s->f_high[0];
    const int odd = s->n_high & 1;
    for (int k = 1; k <= s->n_low; k++) s->f_low[k] = s->f_high[2 * k - odd];
    int nq = nint(s->noise_bands * log2((double)s->k2 / s->kx));
    if (nq < 1) nq = 1;
    if (nq > SBR_MAX_NQ) return false;
    s->n_q = (uint8_t)nq;
    s->f_noise[0] = s->f_low[0];
    int t = 0;
    for (int k = 1; k <= s->n_q; k++) {
        t += (s->n_low - t) / (s->n_q + 1 - k);
        s->f_noise[k] = s->f_low[t];
    }
    if (!make_patches(s)) return false;
    make_limiter(s);
    s->ch[0].noise_index = s->ch[1].noise_index = 0;
    return true;
}

static void read_header(aac_sbr_t *s, aac_bits_t *b) {
    const int8_t old[6] = { s->start_freq, s->stop_freq, s->xover, s->freq_scale, s->alter_scale, s->noise_bands };
    const uint8_t old_limiter = s->limiter_bands;
    s->start = true;
    s->amp_res_header = aac_bits_bit(b);
    s->start_freq = aac_bits_get(b, 4);
    s->stop_freq = aac_bits_get(b, 4);
    s->xover = aac_bits_get(b, 3);
    aac_bits_get(b, 2);
    const bool extra1 = aac_bits_bit(b);
    const bool extra2 = aac_bits_bit(b);
    if (extra1) {
        s->freq_scale = aac_bits_get(b, 2);
        s->alter_scale = aac_bits_bit(b);
        s->noise_bands = aac_bits_get(b, 2);
    } else {
        s->freq_scale = 2;
        s->alter_scale = 1;
        s->noise_bands = 2;
    }
    if (extra2) {
        s->limiter_bands = aac_bits_get(b, 2);
        s->limiter_gains = aac_bits_get(b, 2);
        s->interpol_freq = aac_bits_bit(b);
        s->smoothing_mode = aac_bits_bit(b);
    } else {
        s->limiter_bands = 2;
        s->limiter_gains = 2;
        s->interpol_freq = 1;
        s->smoothing_mode = 1;
    }
    const int8_t now[6] = { s->start_freq, s->stop_freq, s->xover, s->freq_scale, s->alter_scale, s->noise_bands };
    if (memcmp(old, now, sizeof(now))) {
        s->reset = true;
    } else if (s->limiter_bands != old_limiter) {
        make_limiter(s);
    }
}

static bool read_grid(aac_sbr_t *s, aac_bits_t *b, sbr_ch_t *c) {
    static const uint8_t ceil_log2[6] = { 0, 1, 2, 2, 3, 3 };
    const int old_num_env = c->num_env;
    int pointer = 0;
    int trail = 16;
    int n;
    c->freq_res[0] = c->freq_res[c->num_env];
    c->amp_res = s->amp_res_header;
    c->t_env_prev_end = c->t_env[c->num_env];
    c->frame_class = aac_bits_get(b, 2);
    switch (c->frame_class) {
    case FIXFIX: {
        n = 1 << aac_bits_get(b, 2);
        if (n > 4) return false;
        if (n == 1) c->amp_res = 0;
        c->t_env[0] = 0;
        c->t_env[n] = 16;
        const int step = (16 + (n >> 1)) / n;
        for (int i = 0; i < n - 1; i++) c->t_env[i + 1] = c->t_env[i] + step;
        const uint8_t fr = aac_bits_bit(b);
        for (int i = 1; i <= n; i++) c->freq_res[i] = fr;
        break;
    }
    case FIXVAR: {
        trail += aac_bits_get(b, 2);
        const int rel = aac_bits_get(b, 2);
        n = rel + 1;
        c->t_env[0] = 0;
        c->t_env[n] = trail;
        for (int i = 0; i < rel; i++) c->t_env[n - 1 - i] = c->t_env[n - i] - 2 * aac_bits_get(b, 2) - 2;
        pointer = aac_bits_get(b, ceil_log2[n]);
        for (int i = 0; i < n; i++) c->freq_res[n - i] = aac_bits_bit(b);
        break;
    }
    case VARFIX: {
        c->t_env[0] = aac_bits_get(b, 2);
        const int rel = aac_bits_get(b, 2);
        n = rel + 1;
        c->t_env[n] = 16;
        for (int i = 0; i < rel; i++) c->t_env[i + 1] = c->t_env[i] + 2 * aac_bits_get(b, 2) + 2;
        pointer = aac_bits_get(b, ceil_log2[n]);
        for (int i = 1; i <= n; i++) c->freq_res[i] = aac_bits_bit(b);
        break;
    }
    default: {
        c->t_env[0] = aac_bits_get(b, 2);
        trail += aac_bits_get(b, 2);
        const int lead = aac_bits_get(b, 2);
        const int rel = aac_bits_get(b, 2);
        n = lead + rel + 1;
        if (n > SBR_MAX_ENV) return false;
        c->t_env[n] = trail;
        for (int i = 0; i < lead; i++) c->t_env[i + 1] = c->t_env[i] + 2 * aac_bits_get(b, 2) + 2;
        for (int i = 0; i < rel; i++) c->t_env[n - 1 - i] = c->t_env[n - i] - 2 * aac_bits_get(b, 2) - 2;
        pointer = aac_bits_get(b, ceil_log2[n]);
        for (int i = 1; i <= n; i++) c->freq_res[i] = aac_bits_bit(b);
        break;
    }
    }
    c->num_env = (uint8_t)n;
    if (pointer > n + 1) return false;
    for (int i = 1; i <= n; i++) {
        if ((int8_t)c->t_env[i - 1] >= (int8_t)c->t_env[i]) return false;
    }
    c->num_noise = n > 1 ? 2 : 1;
    c->t_q[0] = c->t_env[0];
    c->t_q[c->num_noise] = c->t_env[n];
    if (c->num_noise > 1) {
        int idx;
        if (c->frame_class == FIXFIX) {
            idx = n >> 1;
        } else if (c->frame_class & 1) {
            idx = n - (pointer - 1 > 1 ? pointer - 1 : 1);
        } else {
            idx = !pointer ? 1 : pointer == 1 ? n - 1 : pointer - 1;
        }
        c->t_q[1] = c->t_env[idx];
    }
    c->l_a[0] = c->l_a[1] == old_num_env ? 0 : -1;
    c->l_a[1] = -1;
    if ((c->frame_class & 1) && pointer) {
        c->l_a[1] = (int8_t)(n + 1 - pointer);
    } else if (c->frame_class == VARFIX && pointer > 1) {
        c->l_a[1] = (int8_t)(pointer - 1);
    }
    return true;
}

static void copy_grid(sbr_ch_t *dst, const sbr_ch_t *src) {
    dst->freq_res[0] = dst->freq_res[dst->num_env];
    dst->t_env_prev_end = dst->t_env[dst->num_env];
    dst->l_a[0] = dst->l_a[1] == dst->num_env ? 0 : -1;
    memcpy(dst->freq_res + 1, src->freq_res + 1, SBR_MAX_ENV);
    memcpy(dst->t_env, src->t_env, sizeof(dst->t_env));
    memcpy(dst->t_q, src->t_q, sizeof(dst->t_q));
    dst->num_env = src->num_env;
    dst->amp_res = src->amp_res;
    dst->num_noise = src->num_noise;
    dst->frame_class = src->frame_class;
    dst->l_a[1] = src->l_a[1];
}

static void read_dtdf(aac_bits_t *b, sbr_ch_t *c) {
    for (int i = 0; i < c->num_env; i++) c->df_env[i] = aac_bits_bit(b);
    for (int i = 0; i < c->num_noise; i++) c->df_noise[i] = aac_bits_bit(b);
}

static void read_invf(const aac_sbr_t *s, aac_bits_t *b, sbr_ch_t *c) {
    memcpy(c->invf[1], c->invf[0], SBR_MAX_NQ);
    for (int i = 0; i < s->n_q; i++) c->invf[0][i] = aac_bits_get(b, 2);
}

static bool read_envelope(const aac_sbr_t *s, aac_bits_t *b, sbr_ch_t *c, int ch) {
    const bool bal = s->coupling && ch;
    const int delta = bal ? 2 : 1;
    const int odd = s->n_high & 1;
    int bits, t_huff, f_huff;
    if (bal) {
        bits = c->amp_res ? 5 : 6;
        t_huff = c->amp_res ? SBR_H_T_ENV_BAL_3_0 : SBR_H_T_ENV_BAL_1_5;
        f_huff = c->amp_res ? SBR_H_F_ENV_BAL_3_0 : SBR_H_F_ENV_BAL_1_5;
    } else {
        bits = c->amp_res ? 6 : 7;
        t_huff = c->amp_res ? SBR_H_T_ENV_3_0 : SBR_H_T_ENV_1_5;
        f_huff = c->amp_res ? SBR_H_F_ENV_3_0 : SBR_H_F_ENV_1_5;
    }
    for (int i = 0; i < c->num_env; i++) {
        const int fr = c->freq_res[i + 1];
        const int n = fr ? s->n_high : s->n_low;
        int16_t *cur = c->env_q[i + 1];
        const int16_t *prev = c->env_q[i];
        if (c->df_env[i]) {
            for (int j = 0; j < n; j++) {
                int k;
                if (fr == c->freq_res[i]) {
                    k = j;
                } else if (fr) {
                    k = (j + odd) >> 1;
                } else {
                    k = j ? 2 * j - odd : 0;
                }
                cur[j] = prev[k] + delta * aac_huff(b, t_huff);
                if ((unsigned)cur[j] > 127) return false;
            }
        } else {
            cur[0] = delta * aac_bits_get(b, bits);
            for (int j = 1; j < n; j++) {
                cur[j] = cur[j - 1] + delta * aac_huff(b, f_huff);
                if ((unsigned)cur[j] > 127) return false;
            }
        }
    }
    memcpy(c->env_q[0], c->env_q[c->num_env], sizeof(c->env_q[0]));
    return true;
}

static bool read_noise(const aac_sbr_t *s, aac_bits_t *b, sbr_ch_t *c, int ch) {
    const bool bal = s->coupling && ch;
    const int delta = bal ? 2 : 1;
    const int t_huff = bal ? SBR_H_T_NOISE_BAL_3_0 : SBR_H_T_NOISE_3_0;
    const int f_huff = bal ? SBR_H_F_ENV_BAL_3_0 : SBR_H_F_ENV_3_0;
    for (int i = 0; i < c->num_noise; i++) {
        int16_t *cur = c->noise_q[i + 1];
        if (c->df_noise[i]) {
            for (int j = 0; j < s->n_q; j++) {
                cur[j] = c->noise_q[i][j] + delta * aac_huff(b, t_huff);
                if ((unsigned)cur[j] > 30) return false;
            }
        } else {
            cur[0] = delta * aac_bits_get(b, 5);
            for (int j = 1; j < s->n_q; j++) {
                cur[j] = cur[j - 1] + delta * aac_huff(b, f_huff);
                if ((unsigned)cur[j] > 30) return false;
            }
        }
    }
    memcpy(c->noise_q[0], c->noise_q[c->num_noise], sizeof(c->noise_q[0]));
    return true;
}

static void read_harmonic(const aac_sbr_t *s, aac_bits_t *b, sbr_ch_t *c) {
    c->add_harmonic_flag = aac_bits_bit(b);
    if (c->add_harmonic_flag) {
        for (int i = 0; i < s->n_high; i++) c->add_harmonic[i] = aac_bits_bit(b);
    }
}

static bool read_data(aac_dec_t *d, aac_sbr_t *s, aac_bits_t *b, int element, uint32_t end) {
    sbr_ch_t *c0 = &s->ch[0], *c1 = &s->ch[1];
    if (element == AAC_ID_SCE) {
        if (aac_bits_bit(b)) aac_bits_get(b, 4);
        s->coupling = false;
        if (!read_grid(s, b, c0)) return false;
        read_dtdf(b, c0);
        read_invf(s, b, c0);
        if (!read_envelope(s, b, c0, 0) || !read_noise(s, b, c0, 0)) return false;
        read_harmonic(s, b, c0);
    } else {
        if (aac_bits_bit(b)) aac_bits_get(b, 8);
        s->coupling = aac_bits_bit(b);
        if (s->coupling) {
            if (!read_grid(s, b, c0)) return false;
            copy_grid(c1, c0);
            read_dtdf(b, c0);
            read_dtdf(b, c1);
            read_invf(s, b, c0);
            memcpy(c1->invf[1], c1->invf[0], SBR_MAX_NQ);
            memcpy(c1->invf[0], c0->invf[0], SBR_MAX_NQ);
            if (!read_envelope(s, b, c0, 0) || !read_noise(s, b, c0, 0)) return false;
            if (!read_envelope(s, b, c1, 1) || !read_noise(s, b, c1, 1)) return false;
        } else {
            if (!read_grid(s, b, c0) || !read_grid(s, b, c1)) return false;
            read_dtdf(b, c0);
            read_dtdf(b, c1);
            read_invf(s, b, c0);
            read_invf(s, b, c1);
            if (!read_envelope(s, b, c0, 0) || !read_envelope(s, b, c1, 1)) return false;
            if (!read_noise(s, b, c0, 0) || !read_noise(s, b, c1, 1)) return false;
        }
        read_harmonic(s, b, c0);
        read_harmonic(s, b, c1);
    }
    if (aac_bits_bit(b)) {
        int left = aac_bits_get(b, 4);
        if (left == 15) left += aac_bits_get(b, 8);
        left *= 8;
        while (left > 7) {
            const int id = aac_bits_get(b, 2);
            left -= 2;
            if (id == 2 && s->ps && element == AAC_ID_SCE) {
                left -= aac_ps_parse(s->ps, b, left);
            } else {
                b->pos += left;
                left = 0;
            }
        }
        if (left > 0) b->pos += left;
    }
    (void)d;
    return !aac_bits_overrun(b) && b->pos <= end;
}

esp_err_t aac_sbr_parse(aac_dec_t *d, aac_bits_t *b, int element, unsigned bits, bool crc) {
    aac_sbr_t *s = d->sbr;
    const uint32_t end = b->pos + bits;
    if ((element == AAC_ID_SCE) != (d->core_channels == 1)) return ESP_ERR_INVALID_RESPONSE;
    if (crc) aac_bits_get(b, 10);
    if (aac_bits_bit(b)) read_header(s, b);
    if (s->reset && !make_tables(s)) {
        turn_off(s);
        return ESP_ERR_INVALID_RESPONSE;
    }
    if (!s->start) return ESP_OK;
    s->element = element;
    if (!read_data(d, s, b, element, end)) {
        turn_off(s);
        return ESP_ERR_INVALID_RESPONSE;
    }
    s->have_data = true;
    return ESP_OK;
}

void aac_sbr_frame_start(aac_dec_t *d) {
    d->sbr->have_data = false;
    d->sbr->reset = false;
}

static float exp2_half(int e) {
    return (e & 1) ? ldexpf(1.41421356237f, e >> 1) : ldexpf(1.0f, e >> 1);
}

static void dequant(aac_sbr_t *s, int channels) {
    const float scale = 256.0f;
    if (channels == 2 && s->coupling) {
        sbr_ch_t *c0 = &s->ch[0], *c1 = &s->ch[1];
        const int pan = c0->amp_res ? 12 : 24;
        for (int e = 0; e < c0->num_env; e++) {
            const int n = c0->freq_res[e + 1] ? s->n_high : s->n_low;
            for (int k = 0; k < n; k++) {
                const int e0 = c0->env_q[e + 1][k], e1 = c1->env_q[e + 1][k];
                float t1, t2;
                if (c0->amp_res) {
                    t1 = ldexpf(1.0f, e0 + 7);
                    t2 = ldexpf(1.0f, pan - e1);
                } else {
                    t1 = exp2_half(e0 + 14);
                    t2 = exp2_half(pan - e1);
                }
                if (t1 > 1e20f) t1 = 0.0f;
                const float f = t1 / (1.0f + t2);
                c0->env[e][k] = f * scale;
                c1->env[e][k] = f * t2 * scale;
            }
        }
        for (int e = 0; e < c0->num_noise; e++) {
            for (int k = 0; k < s->n_q; k++) {
                const float t1 = ldexpf(1.0f, 6 - c0->noise_q[e + 1][k] + 1);
                const float t2 = ldexpf(1.0f, 12 - c1->noise_q[e + 1][k]);
                const float f = t1 / (1.0f + t2);
                c0->noise[e][k] = f;
                c1->noise[e][k] = f * t2;
            }
        }
        return;
    }
    for (int ch = 0; ch < channels; ch++) {
        sbr_ch_t *c = &s->ch[ch];
        for (int e = 0; e < c->num_env; e++) {
            const int n = c->freq_res[e + 1] ? s->n_high : s->n_low;
            for (int k = 0; k < n; k++) {
                const int q = c->env_q[e + 1][k];
                float v = c->amp_res ? ldexpf(1.0f, q + 6) : exp2_half(q + 12);
                if (v > 1e20f) v = 0.0f;
                c->env[e][k] = v * scale;
            }
        }
        for (int e = 0; e < c->num_noise; e++) {
            for (int k = 0; k < s->n_q; k++) c->noise[e][k] = ldexpf(1.0f, 6 - c->noise_q[e + 1][k]);
        }
    }
}

static void build_row(aac_sbr_t *s, sbr_ch_t *c, const sbr_hf_t *w, int l, int i_temp, int32_t *row) {
    memset(row, 0, 128 * sizeof(int32_t));
    if (l < i_temp) {
        memcpy(row, c->x_low + (l + SBR_HF_ADJ) * 64, s->kx_prev * 2 * sizeof(int32_t));
        memcpy(row + 2 * s->kx_prev, c->y_tail + l * 128 + 2 * s->kx_prev, s->m_prev * 2 * sizeof(int32_t));
    } else {
        memcpy(row, c->x_low + (l + SBR_HF_ADJ) * 64, s->kx * 2 * sizeof(int32_t));
    }
    if (s->start && l < SBR_SLOTS && l >= 2 * c->t_env[0] && l < 2 * c->t_env[c->num_env]) {
        sbr_hf_slot(s, c, w, l, l >= i_temp ? row : NULL);
    }
}

static int start_channel(aac_dec_t *d, int ch) {
    aac_sbr_t *s = d->sbr;
    sbr_ch_t *c = &s->ch[ch];
    aac_filterbank(d, &d->ch[ch]);
    aac_prof(d, AUDF_AAC_PROF_FILTERBANK);
    sbr_qmf_analysis(c, d->ch[ch].time, s->kx > s->kx_prev ? s->kx : s->kx_prev, d->scratch);
    for (int l = 0; l < SBR_HF_GEN; l++) {
        memset(c->x_low + l * 64 + 2 * s->kx_prev, 0, (32 - s->kx_prev) * 2 * sizeof(int32_t));
    }
    aac_prof(d, AUDF_AAC_PROF_QMF_ANALYSIS);
    if (s->start) sbr_hf_prepare(s, c, s->hf[ch]);
    aac_prof(d, AUDF_AAC_PROF_SBR);
    const int i_temp = 2 * c->t_env_prev_end - SBR_SLOTS;
    return i_temp > 0 ? i_temp : 0;
}

static void finish_channel(aac_sbr_t *s, int ch) {
    sbr_ch_t *c = &s->ch[ch];
    if (!s->start) return;
    for (int l = SBR_SLOTS; l < 2 * c->t_env[c->num_env]; l++) {
        if (l >= 2 * c->t_env[0]) sbr_hf_slot(s, c, s->hf[ch], l, c->y_tail + (l - SBR_SLOTS) * 128);
    }
    sbr_hf_finish(s, c, s->hf[ch]);
}

void aac_sbr_apply(aac_dec_t *d, int16_t *out) {
    aac_sbr_t *s = d->sbr;
    if (s->start && !s->have_data) turn_off(s);
    const int channels = d->core_channels;
    if (s->start) dequant(s, channels);
    int32_t *tmp = d->scratch;
    int32_t *acc[2] = { d->scratch + 768, d->scratch + 1024 };
    int32_t *rows = d->scratch + 1280;
    if (d->ps_on) {
        const int i_temp = start_channel(d, 0);
        const bool ps = s->start && aac_ps_ready(s->ps);
        int32_t *l_rows = rows, *r_rows = rows + 512;
        if (ps) aac_ps_begin(s->ps, s->ch[0].x_low + SBR_HF_ADJ * 64, s->kx + s->m);
        aac_prof(d, AUDF_AAC_PROF_PS);
        const int32_t *const r0[4] = { l_rows, l_rows + 128, l_rows + 256, l_rows + 384 };
        const int32_t *const r1[4] = { r_rows, r_rows + 128, r_rows + 256, r_rows + 384 };
        for (int l = 0; l < SBR_SLOTS; l += 4) {
            for (int i = 0; i < 4; i++) build_row(s, &s->ch[0], s->hf[0], l + i, i_temp, l_rows + i * 128);
            aac_prof(d, AUDF_AAC_PROF_SBR);
            if (ps) {
                aac_ps_slots(s->ps, l_rows, r_rows, l, 4);
            } else {
                memcpy(r_rows, l_rows, 4 * 128 * sizeof(int32_t));
            }
            aac_prof(d, AUDF_AAC_PROF_PS);
            sbr_qmf_synthesis(&s->ch[0], r0, acc[0], tmp);
            sbr_qmf_synthesis(&s->ch[1], r1, acc[1], tmp);
            for (int i = 0; i < 4; i++) aac_k_pack(out + (l + i) * 128, acc[0] + 64 * i, acc[1] + 64 * i, 64, 7);
            aac_prof(d, AUDF_AAC_PROF_QMF_SYNTHESIS);
        }
        if (ps) aac_ps_end(s->ps);
        finish_channel(s, 0);
        aac_prof(d, AUDF_AAC_PROF_SBR);
    } else {
        int i_temp[2];
        for (int ch = 0; ch < channels; ch++) i_temp[ch] = start_channel(d, ch);
        const int32_t *const r[4] = { rows, rows + 128, rows + 256, rows + 384 };
        for (int l = 0; l < SBR_SLOTS; l += 4) {
            for (int ch = 0; ch < channels; ch++) {
                for (int i = 0; i < 4; i++) build_row(s, &s->ch[ch], s->hf[ch], l + i, i_temp[ch], rows + i * 128);
                aac_prof(d, AUDF_AAC_PROF_SBR);
                sbr_qmf_synthesis(&s->ch[ch], r, acc[ch], tmp);
                aac_prof(d, AUDF_AAC_PROF_QMF_SYNTHESIS);
            }
            for (int i = 0; i < 4; i++) {
                const int32_t *right = channels == 2 ? acc[1] + 64 * i : NULL;
                aac_k_pack(out + (l + i) * 64 * channels, acc[0] + 64 * i, right, 64, 7);
            }
        }
        for (int ch = 0; ch < channels; ch++) finish_channel(s, ch);
        aac_prof(d, AUDF_AAC_PROF_SBR);
    }
    s->kx_prev = s->kx;
    s->m_prev = s->m;
}

esp_err_t aac_sbr_create(aac_dec_t *d) {
    const uint32_t caps = d->alloc_caps;
    aac_sbr_t *s = audf_calloc(1, sizeof(*s), caps);
    if (!s) return ESP_ERR_NO_MEM;
    d->sbr = s;
    s->rate = d->out_rate;
    s->kx_prev = 32;
    turn_off(s);
    const int synth = d->base.channels;
    for (int ch = 0; ch < synth; ch++) {
        sbr_ch_t *c = &s->ch[ch];
        if (ch < d->core_channels) {
            c->ana_hi = audf_malloc_aligned(16, 2 * SBR_ANA_LEN * sizeof(int16_t), caps);
            c->x_low = audf_calloc(SBR_XLOW_SLOTS * 64, sizeof(int32_t), caps);
            c->y_tail = audf_calloc(6 * 128, sizeof(int32_t), caps);
            if (!c->ana_hi || !c->x_low || !c->y_tail) return ESP_ERR_NO_MEM;
            c->ana_lo = c->ana_hi + SBR_ANA_LEN;
            memset(c->ana_hi, 0, 2 * SBR_ANA_LEN * sizeof(int16_t));
        }
        c->v_hi = audf_malloc_aligned(16, 2 * SBR_V_LEN * sizeof(int16_t), caps);
        if (!c->v_hi) return ESP_ERR_NO_MEM;
        c->v_lo = c->v_hi + SBR_V_LEN;
        memset(c->v_hi, 0, 2 * SBR_V_LEN * sizeof(int16_t));
        c->v_off = SBR_V_LEN - 1280;
        c->l_a[1] = -1;
    }
    for (int ch = 0; ch < d->core_channels; ch++) {
        if (!(s->hf[ch] = audf_calloc(1, sbr_hf_size(), caps))) return ESP_ERR_NO_MEM;
    }
    if (!d->ps_on) return ESP_OK;
    return aac_ps_create(&s->ps, caps);
}

void aac_sbr_reset(aac_dec_t *d) {
    aac_sbr_t *s = d->sbr;
    for (int ch = 0; ch < 2; ch++) {
        sbr_ch_t *c = &s->ch[ch];
        int16_t *ana_hi = c->ana_hi, *ana_lo = c->ana_lo, *v_hi = c->v_hi, *v_lo = c->v_lo;
        int32_t *x_low = c->x_low, *y_tail = c->y_tail;
        memset(c, 0, sizeof(*c));
        c->ana_hi = ana_hi;
        c->ana_lo = ana_lo;
        c->v_hi = v_hi;
        c->v_lo = v_lo;
        c->x_low = x_low;
        c->y_tail = y_tail;
        c->l_a[1] = -1;
        c->v_off = SBR_V_LEN - 1280;
        if (ana_hi) memset(ana_hi, 0, 2 * SBR_ANA_LEN * sizeof(int16_t));
        if (x_low) memset(x_low, 0, SBR_XLOW_SLOTS * 64 * sizeof(int32_t));
        if (y_tail) memset(y_tail, 0, 6 * 128 * sizeof(int32_t));
        if (v_hi) memset(v_hi, 0, 2 * SBR_V_LEN * sizeof(int16_t));
    }
    s->have_data = false;
    s->m_prev = 0;
    s->kx_prev = s->kx;
    if (s->ps) aac_ps_reset(s->ps);
}

void aac_sbr_destroy(aac_dec_t *d) {
    aac_sbr_t *s = d->sbr;
    if (!s) return;
    for (int ch = 0; ch < 2; ch++) {
        audf_free(s->ch[ch].ana_hi);
        audf_free(s->ch[ch].x_low);
        audf_free(s->ch[ch].y_tail);
        audf_free(s->ch[ch].v_hi);
    }
    audf_free(s->hf[0]);
    audf_free(s->hf[1]);
    aac_ps_destroy(s->ps);
    audf_free(s);
    d->sbr = NULL;
}
