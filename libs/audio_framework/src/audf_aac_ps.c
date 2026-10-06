/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <math.h>
#include <string.h>

#include "audf_aac_kernels.h"
#include "audf_aac_ps.h"
#include "audf_alloc.h"

#define PS_SLOTS     32
#define PS_BANDS     71
#define PS_PAR       20
#define PS_AP_BANDS  30
#define PS_SHORT     42
#define PS_DELAY     14
#define PS_MAX_ENV   5
#define PS_MAX_PAR   34
#define PS_IPDOPD    11
#define PS_LANES     72
#define PS_GROUPS    (PS_LANES / 4)
#define PS_D1_GROUP  11

struct aac_ps {
    int32_t l[2 * PS_LANES] __attribute__((aligned(16)));
    int32_t r[2 * PS_LANES] __attribute__((aligned(16)));
    int32_t d2[2][64] __attribute__((aligned(16)));
    int32_t ap[3][8][64] __attribute__((aligned(16)));
    int16_t gain[PS_GROUPS * 8] __attribute__((aligned(16)));
    int32_t mix_acc[PS_GROUPS * 32] __attribute__((aligned(16)));
    int32_t mix_step[PS_GROUPS * 32] __attribute__((aligned(16)));
    int32_t d14[PS_DELAY][2][PS_SHORT - PS_AP_BANDS];
    int32_t d1[8 * (PS_GROUPS - PS_D1_GROUP) + 2] __attribute__((aligned(16)));
    bool    start;
    bool    enable_iid;
    bool    enable_icc;
    bool    enable_ext;
    bool    enable_ipdopd;
    uint8_t iid_quant;
    uint8_t nr_iid;
    uint8_t nr_icc;
    uint8_t nr_ipdopd;
    uint8_t icc_mode;
    uint8_t num_env;
    uint8_t num_env_old;
    int8_t  border[PS_MAX_ENV + 1];
    int8_t  iid[PS_MAX_ENV][PS_MAX_PAR];
    int8_t  icc[PS_MAX_ENV][PS_MAX_PAR];
    int8_t  ipd[PS_MAX_ENV][PS_MAX_PAR];
    int8_t  opd[PS_MAX_ENV][PS_MAX_PAR];
    float   h[4][PS_MAX_ENV + 1][PS_PAR];
    float   hi[4][PS_MAX_ENV + 1][PS_PAR];
    uint8_t ipd_hist[PS_IPDOPD];
    uint8_t opd_hist[PS_IPDOPD];
    float   in_buf[3][44][2];
    uint8_t pos14;
    uint8_t pos_ap;
    int8_t  env;
    float   peak_decay_nrg[PS_PAR];
    float   power_smooth[PS_PAR];
    float   peak_decay_diff_smooth[PS_PAR];
};

static const uint8_t s_num_env[2][4] = { { 0, 1, 2, 4 }, { 1, 2, 3, 4 } };
static const uint8_t s_nr_iidicc[6] = { 10, 20, 34, 10, 20, 34 };
static const uint8_t s_nr_ipdopd[6] = { 5, 11, 17, 5, 11, 17 };

static bool read_par(aac_bits_t *b, int8_t (*par)[PS_MAX_PAR], int table, int e, bool dt, int num,
                     int num_env_old, int limit, int mask) {
    if (dt) {
        int prev = e ? e - 1 : num_env_old - 1;
        if (prev < 0) prev = 0;
        for (int i = 0; i < num; i++) {
            int v = par[prev][i] + aac_huff(b, table);
            if (mask) v &= mask;
            if (limit >= 0 && (v > limit || v < -limit)) return false;
            par[e][i] = (int8_t)v;
        }
    } else {
        int v = 0;
        for (int i = 0; i < num; i++) {
            v += aac_huff(b, table);
            if (mask) v &= mask;
            if (limit >= 0 && (v > limit || v < -limit)) return false;
            par[e][i] = (int8_t)v;
        }
    }
    return true;
}

static int read_extension(aac_ps_t *ps, aac_bits_t *b, int id) {
    const uint32_t start = b->pos;
    if (id) return 0;
    ps->enable_ipdopd = aac_bits_bit(b);
    if (ps->enable_ipdopd) {
        for (int e = 0; e < ps->num_env; e++) {
            bool dt = aac_bits_bit(b);
            read_par(b, ps->ipd, dt ? PS_H_IPD_DT : PS_H_IPD_DF, e, dt, ps->nr_ipdopd, ps->num_env_old, -1, 7);
            dt = aac_bits_bit(b);
            read_par(b, ps->opd, dt ? PS_H_OPD_DT : PS_H_OPD_DF, e, dt, ps->nr_ipdopd, ps->num_env_old, -1, 7);
        }
    }
    aac_bits_bit(b);
    return (int)(b->pos - start);
}

static bool read_data(aac_ps_t *ps, aac_bits_t *b, bool *header) {
    *header = aac_bits_bit(b);
    if (*header) {
        ps->enable_iid = aac_bits_bit(b);
        if (ps->enable_iid) {
            const int mode = aac_bits_get(b, 3);
            if (mode > 5) return false;
            ps->nr_iid = s_nr_iidicc[mode];
            ps->iid_quant = mode > 2;
            ps->nr_ipdopd = s_nr_ipdopd[mode];
        }
        ps->enable_icc = aac_bits_bit(b);
        if (ps->enable_icc) {
            ps->icc_mode = aac_bits_get(b, 3);
            if (ps->icc_mode > 5) return false;
            ps->nr_icc = s_nr_iidicc[ps->icc_mode];
        }
        ps->enable_ext = aac_bits_bit(b);
    }
    const int frame_class = aac_bits_bit(b);
    ps->num_env_old = ps->num_env;
    ps->num_env = s_num_env[frame_class][aac_bits_get(b, 2)];
    ps->border[0] = -1;
    if (frame_class) {
        for (int e = 1; e <= ps->num_env; e++) {
            ps->border[e] = (int8_t)aac_bits_get(b, 5);
            if (ps->border[e] < ps->border[e - 1]) return false;
        }
    } else {
        const int shift = ps->num_env == 4 ? 2 : ps->num_env == 2 ? 1 : 0;
        for (int e = 1; e <= ps->num_env; e++) ps->border[e] = (int8_t)((e * PS_SLOTS >> shift) - 1);
    }
    if (ps->enable_iid) {
        for (int e = 0; e < ps->num_env; e++) {
            const bool dt = aac_bits_bit(b);
            static const int tables[4] = { PS_H_IID_DF0, PS_H_IID_DF1, PS_H_IID_DT0, PS_H_IID_DT1 };
            if (!read_par(b, ps->iid, tables[2 * dt + ps->iid_quant], e, dt, ps->nr_iid, ps->num_env_old,
                          7 + 8 * ps->iid_quant, 0)) {
                return false;
            }
        }
    } else {
        memset(ps->iid, 0, sizeof(ps->iid));
    }
    if (ps->enable_icc) {
        for (int e = 0; e < ps->num_env; e++) {
            const bool dt = aac_bits_bit(b);
            if (!read_par(b, ps->icc, dt ? PS_H_ICC_DT : PS_H_ICC_DF, e, dt, ps->nr_icc, ps->num_env_old, 7, 0)) {
                return false;
            }
            for (int i = 0; i < ps->nr_icc; i++) {
                if (ps->icc[e][i] < 0) return false;
            }
        }
    } else {
        memset(ps->icc, 0, sizeof(ps->icc));
    }
    if (ps->enable_ext) {
        int cnt = aac_bits_get(b, 4);
        if (cnt == 15) cnt += aac_bits_get(b, 8);
        cnt *= 8;
        while (cnt > 7) {
            const int id = aac_bits_get(b, 2);
            cnt -= 2 + read_extension(ps, b, id);
        }
        if (cnt < 0) return false;
        b->pos += cnt;
    }
    if (!ps->num_env || ps->border[ps->num_env] < PS_SLOTS - 1) {
        const int source = ps->num_env ? ps->num_env - 1 : ps->num_env_old - 1;
        if (source >= 0 && source != ps->num_env) {
            if (ps->enable_iid) memcpy(ps->iid[ps->num_env], ps->iid[source], sizeof(ps->iid[0]));
            if (ps->enable_icc) memcpy(ps->icc[ps->num_env], ps->icc[source], sizeof(ps->icc[0]));
            if (ps->enable_ipdopd) {
                memcpy(ps->ipd[ps->num_env], ps->ipd[source], sizeof(ps->ipd[0]));
                memcpy(ps->opd[ps->num_env], ps->opd[source], sizeof(ps->opd[0]));
            }
        }
        ps->num_env++;
        ps->border[ps->num_env] = PS_SLOTS - 1;
    }
    if (!ps->enable_ipdopd) {
        memset(ps->ipd, 0, sizeof(ps->ipd));
        memset(ps->opd, 0, sizeof(ps->opd));
    }
    return true;
}

int aac_ps_parse(aac_ps_t *ps, aac_bits_t *b, int bits) {
    const uint32_t start = b->pos;
    bool header = false;
    if (read_data(ps, b, &header) && (int)(b->pos - start) <= bits) {
        if (header) ps->start = true;
        return (int)(b->pos - start);
    }
    ps->start = false;
    memset(ps->iid, 0, sizeof(ps->iid));
    memset(ps->icc, 0, sizeof(ps->icc));
    b->pos = start + bits;
    return bits;
}

bool aac_ps_ready(const aac_ps_t *ps) {
    return ps->start;
}

static void hybrid8(float (*out)[2], const float (*in)[2]) {
    float t[8][2];
    for (int q = 0; q < 8; q++) {
        const float (*f)[2] = ps_f20[q];
        float re = f[6][0] * in[6][0];
        float im = f[6][0] * in[6][1];
        for (int j = 0; j < 6; j++) {
            re += f[j][0] * (in[j][0] + in[12 - j][0]) - f[j][1] * (in[j][1] - in[12 - j][1]);
            im += f[j][0] * (in[j][1] + in[12 - j][1]) + f[j][1] * (in[j][0] - in[12 - j][0]);
        }
        t[q][0] = re;
        t[q][1] = im;
    }
    out[0][0] = t[6][0];
    out[0][1] = t[6][1];
    out[1][0] = t[7][0];
    out[1][1] = t[7][1];
    out[2][0] = t[0][0];
    out[2][1] = t[0][1];
    out[3][0] = t[1][0];
    out[3][1] = t[1][1];
    out[4][0] = t[2][0] + t[5][0];
    out[4][1] = t[2][1] + t[5][1];
    out[5][0] = t[3][0] + t[4][0];
    out[5][1] = t[3][1] + t[4][1];
}

static void hybrid2(float (*out)[2], const float (*in)[2], int reverse) {
    static const float g[7] = { 0.0f, 0.01899487526049f, 0.0f, -0.07293139167538f, 0.0f, 0.30596630545168f, 0.5f };
    const float re_in = g[6] * in[6][0];
    const float im_in = g[6] * in[6][1];
    float re_op = 0.0f, im_op = 0.0f;
    for (int j = 0; j < 6; j += 2) {
        re_op += g[j + 1] * (in[j + 1][0] + in[12 - j - 1][0]);
        im_op += g[j + 1] * (in[j + 1][1] + in[12 - j - 1][1]);
    }
    out[reverse][0] = re_in + re_op;
    out[reverse][1] = im_in + im_op;
    out[!reverse][0] = re_in - re_op;
    out[!reverse][1] = im_in - im_op;
}

/* Lane 10 stays empty so that QMF bands 4-63 fill whole groups. */
static inline int lane(int k, int c) {
    const int p = k < 10 ? k : k + 1;
    return (p >> 2) * 8 + c * 4 + (p & 3);
}

static inline int32_t to_fixed(float v) {
    if (v > 1073741824.0f) return 1073741824;
    if (v < -1073741824.0f) return -1073741824;
    return (int32_t)lrintf(v);
}

static void analysis(aac_ps_t *ps, const int32_t *row, int n) {
    float hy[10][2];
    hybrid8(hy, (const float (*)[2])ps->in_buf[0] + n);
    hybrid2(hy + 6, (const float (*)[2])ps->in_buf[1] + n, 1);
    hybrid2(hy + 8, (const float (*)[2])ps->in_buf[2] + n, 0);
    for (int k = 0; k < 10; k++) {
        ps->l[lane(k, 0)] = to_fixed(hy[k][0]);
        ps->l[lane(k, 1)] = to_fixed(hy[k][1]);
    }
    ps->l[lane(10, 0)] = row[6];
    ps->l[lane(10, 1)] = row[7];
    aac_k_ps_unzip(ps->l + 24, row + 8, PS_GROUPS - 3);
}

static void decorrelate(aac_ps_t *ps, int n) {
    static const uint8_t first[PS_PAR - 1] = { 4, 5, 6, 7, 8, 9, 11, 12, 13, 14, 15, 16, 17, 19, 22, 26, 31, 43, 72 };
    float nrg[PS_LANES];
    float power[PS_PAR];
    for (int g = 0; g < PS_GROUPS; g++) {
        const int32_t *v = ps->l + 8 * g;
        for (int n = 0; n < 4; n++) {
            const float re = (float)v[n], im = (float)v[4 + n];
            nrg[4 * g + n] = re * re + im * im;
        }
    }
    power[0] = nrg[1] + nrg[2];
    power[1] = nrg[0] + nrg[3];
    for (int i = 2; i < PS_PAR; i++) {
        float sum = 0.0f;
        for (int p = first[i - 2]; p < first[i - 1]; p++) sum += nrg[p];
        power[i] = sum;
    }
    for (int i = 0; i < PS_PAR; i++) {
        const float decayed = 0.76592833836465f * ps->peak_decay_nrg[i];
        ps->peak_decay_nrg[i] = decayed > power[i] ? decayed : power[i];
        ps->power_smooth[i] += 0.25f * (power[i] - ps->power_smooth[i]);
        ps->peak_decay_diff_smooth[i] += 0.25f * (ps->peak_decay_nrg[i] - power[i] - ps->peak_decay_diff_smooth[i]);
        const float denom = 1.5f * ps->peak_decay_diff_smooth[i];
        const float g = denom > ps->power_smooth[i] ? ps->power_smooth[i] / denom : 1.0f;
        const int16_t q = (int16_t)lrintf(g * 32767.0f);
        if (i < 2) {
            const int a = i ? 0 : 1, b = i ? 3 : 2;
            ps->gain[a] = ps->gain[4 + a] = ps->gain[b] = ps->gain[4 + b] = q;
            continue;
        }
        for (int p = first[i - 2]; p < first[i - 1]; p++) {
            int16_t *v = ps->gain + 8 * (p >> 2) + (p & 3);
            v[0] = v[4] = q;
        }
    }
    aac_k_ps_allpass(ps->r, ps->d2[n & 1], ps->ap[0][0], ps->pos_ap);
    memcpy(ps->d2[n & 1], ps->l, sizeof(ps->d2[0]));
    ps->pos_ap = (ps->pos_ap + 1) & 7;
    const int p = ps->pos14;
    for (int c = 0; c < 2; c++) {
        int32_t *d = ps->d14[p][c];
        for (int k = PS_AP_BANDS; k < PS_SHORT; k++) {
            ps->r[lane(k, c)] = d[k - PS_AP_BANDS];
            d[k - PS_AP_BANDS] = ps->l[lane(k, c)];
        }
        int32_t *d1 = ps->d1 + 8 * (PS_GROUPS - PS_D1_GROUP) + c;
        ps->r[lane(PS_SHORT, c)] = *d1;
        *d1 = ps->l[lane(PS_SHORT, c)];
    }
    const size_t d1_bytes = 8 * (PS_GROUPS - PS_D1_GROUP) * sizeof(int32_t);
    memcpy(ps->r + 8 * PS_D1_GROUP, ps->d1, d1_bytes);
    memcpy(ps->d1, ps->l + 8 * PS_D1_GROUP, d1_bytes);
    ps->pos14 = p == PS_DELAY - 1 ? 0 : p + 1;
    aac_k_ps_gain(ps->r, ps->gain, PS_GROUPS);
}

static void map_to_20(int8_t *dst, const int8_t *par, int num) {
    if (num == 10) {
        for (int b = 9; b >= 0; b--) dst[2 * b + 1] = dst[2 * b] = par[b];
    } else if (num == 5) {
        dst[10] = 0;
        for (int b = 4; b >= 0; b--) dst[2 * b + 1] = dst[2 * b] = par[b];
    } else if (num == 34 || num == 17) {
        dst[0] = (2 * par[0] + par[1]) / 3;
        dst[1] = (par[1] + 2 * par[2]) / 3;
        dst[2] = (2 * par[3] + par[4]) / 3;
        dst[3] = (par[4] + 2 * par[5]) / 3;
        dst[4] = (par[6] + par[7]) / 2;
        dst[5] = (par[8] + par[9]) / 2;
        dst[6] = par[10];
        dst[7] = par[11];
        dst[8] = (par[12] + par[13]) / 2;
        dst[9] = (par[14] + par[15]) / 2;
        dst[10] = par[16];
        dst[11] = par[17];
        dst[12] = par[18];
        dst[13] = par[19];
        dst[14] = (par[20] + par[21]) / 2;
        dst[15] = (par[22] + par[23]) / 2;
        dst[16] = (par[24] + par[25]) / 2;
        dst[17] = (par[26] + par[27]) / 2;
        dst[18] = (par[28] + par[29] + par[30] + par[31]) / 4;
        dst[19] = (par[32] + par[33]) / 2;
    } else {
        memcpy(dst, par, PS_PAR);
    }
}

static void mixing_matrices(aac_ps_t *ps) {
    const float (*lut)[8][4] = ps->icc_mode < 3 ? ps_ha : ps_hb;
    if (ps->num_env_old) {
        for (int j = 0; j < 4; j++) {
            memcpy(ps->h[j][0], ps->h[j][ps->num_env_old], sizeof(ps->h[j][0]));
            memcpy(ps->hi[j][0], ps->hi[j][ps->num_env_old], sizeof(ps->hi[j][0]));
        }
    }
    const bool ipdopd = ps->enable_ipdopd;
    for (int e = 0; e < ps->num_env; e++) {
        int8_t iid[PS_PAR], icc[PS_PAR], ipd[PS_PAR], opd[PS_PAR];
        map_to_20(iid, ps->iid[e], ps->nr_iid);
        map_to_20(icc, ps->icc[e], ps->nr_icc);
        if (ipdopd) {
            map_to_20(ipd, ps->ipd[e], ps->nr_ipdopd);
            map_to_20(opd, ps->opd[e], ps->nr_ipdopd);
        }
        for (int b = 0; b < PS_PAR; b++) {
            const float *h = lut[iid[b] + 7 + 23 * ps->iid_quant][icc[b]];
            float h11 = h[0], h12 = h[1], h21 = h[2], h22 = h[3];
            if (ipdopd && b < PS_IPDOPD) {
                const int opd_idx = ps->opd_hist[b] * 8 + opd[b];
                const int ipd_idx = ps->ipd_hist[b] * 8 + ipd[b];
                const float opd_re = ps_pd_smooth[opd_idx][0], opd_im = ps_pd_smooth[opd_idx][1];
                const float ipd_re = ps_pd_smooth[ipd_idx][0], ipd_im = ps_pd_smooth[ipd_idx][1];
                ps->opd_hist[b] = opd_idx & 0x3f;
                ps->ipd_hist[b] = ipd_idx & 0x3f;
                const float adj_re = opd_re * ipd_re + opd_im * ipd_im;
                const float adj_im = opd_im * ipd_re - opd_re * ipd_im;
                ps->hi[0][e + 1][b] = h11 * opd_im;
                ps->hi[1][e + 1][b] = h12 * adj_im;
                ps->hi[2][e + 1][b] = h21 * opd_im;
                ps->hi[3][e + 1][b] = h22 * adj_im;
                h11 *= opd_re;
                h12 *= adj_re;
                h21 *= opd_re;
                h22 *= adj_re;
            }
            ps->h[0][e + 1][b] = h11;
            ps->h[1][e + 1][b] = h12;
            ps->h[2][e + 1][b] = h21;
            ps->h[3][e + 1][b] = h22;
        }
    }
}

static inline int32_t q30(float v) {
    v *= 1073741824.0f;
    if (v > 2147483520.0f) return 2147483520;
    if (v < -2147483648.0f) return INT32_MIN;
    return (int32_t)lrintf(v);
}

static void mix_start(aac_ps_t *ps, int e) {
    static const uint8_t order[4] = { 0, 2, 1, 3 };
    const float width = 1.0f / (float)(ps->border[e + 1] - ps->border[e]);
    for (int k = 0; k < PS_BANDS; k++) {
        const int b = ps_k_to_i[k];
        const int p = k < 10 ? k : k + 1;
        int32_t *acc = ps->mix_acc + 32 * (p >> 2) + (p & 3);
        int32_t *step = ps->mix_step + 32 * (p >> 2) + (p & 3);
        for (int j = 0; j < 4; j++) {
            const float h0 = ps->h[order[j]][e][b], h1 = ps->h[order[j]][e + 1][b];
            acc[8 * j] = q30(h0);
            step[8 * j] = q30((h1 - h0) * width);
            float g0 = 0.0f, g1 = 0.0f;
            if (ps->enable_ipdopd) {
                g0 = k <= 1 ? -ps->hi[order[j]][e][b] : ps->hi[order[j]][e][b];
                g1 = ps->hi[order[j]][e + 1][b];
            }
            acc[8 * j + 4] = q30(g0);
            step[8 * j + 4] = q30((g1 - g0) * width);
        }
    }
}

static void synthesis(const int32_t *in, int32_t *dst) {
    int32_t re = 0, im = 0;
    for (int k = 0; k < 6; k++) {
        re += in[lane(k, 0)];
        im += in[lane(k, 1)];
    }
    dst[0] = re;
    dst[1] = im;
    dst[2] = in[lane(6, 0)] + in[lane(7, 0)];
    dst[3] = in[lane(6, 1)] + in[lane(7, 1)];
    dst[4] = in[lane(8, 0)] + in[lane(9, 0)];
    dst[5] = in[lane(8, 1)] + in[lane(9, 1)];
    dst[6] = in[lane(10, 0)];
    dst[7] = in[lane(10, 1)];
    aac_k_ps_zip(dst + 8, in + 24, PS_GROUPS - 3);
}

void aac_ps_begin(aac_ps_t *ps, const int32_t *x_low, int top) {
    top += PS_BANDS - 64;
    for (int k = top; k < PS_BANDS; k++) {
        for (int c = 0; c < 2; c++) {
            if (k < PS_AP_BANDS) {
                ps->d2[0][lane(k, c)] = ps->d2[1][lane(k, c)] = 0;
                for (int m = 0; m < 3; m++) {
                    for (int i = 0; i < 8; i++) ps->ap[m][i][lane(k, c)] = 0;
                }
            } else if (k < PS_SHORT) {
                for (int i = 0; i < PS_DELAY; i++) ps->d14[i][c][k - PS_AP_BANDS] = 0;
            } else if (k == PS_SHORT) {
                ps->d1[8 * (PS_GROUPS - PS_D1_GROUP) + c] = 0;
            } else {
                ps->d1[lane(k, c) - 8 * PS_D1_GROUP] = 0;
            }
        }
    }
    for (int i = 0; i < 3; i++) {
        for (int j = 0; j < 38; j++) {
            ps->in_buf[i][j + 6][0] = (float)x_low[j * 64 + 2 * i];
            ps->in_buf[i][j + 6][1] = (float)x_low[j * 64 + 2 * i + 1];
        }
    }
    mixing_matrices(ps);
    int e = 0;
    while (e < ps->num_env && ps->border[e + 1] < 0) e++;
    if (e < ps->num_env) mix_start(ps, e);
    ps->env = (int8_t)e;
}

void aac_ps_slots(aac_ps_t *ps, int32_t *l, int32_t *r, int n0, int count) {
    int e = ps->env;
    for (int n = n0; n < n0 + count; n++, l += 128, r += 128) {
        analysis(ps, l, n);
        decorrelate(ps, n);
        while (e < ps->num_env && n > ps->border[e + 1]) {
            e++;
            while (e < ps->num_env && ps->border[e + 1] == ps->border[e]) e++;
            if (e < ps->num_env) mix_start(ps, e);
        }
        if (e < ps->num_env && ps->enable_ipdopd) {
            aac_k_ps_mix(ps->l, ps->r, ps->mix_acc, ps->mix_step, PS_GROUPS);
        } else if (e < ps->num_env) {
            aac_k_ps_mix_real(ps->l, ps->r, ps->mix_acc, ps->mix_step, PS_GROUPS);
        }
        synthesis(ps->l, l);
        synthesis(ps->r, r);
    }
    ps->env = (int8_t)e;
}

void aac_ps_end(aac_ps_t *ps) {
    for (int i = 0; i < 3; i++) memcpy(ps->in_buf[i], ps->in_buf[i] + 32, 6 * sizeof(ps->in_buf[i][0]));
}

void aac_ps_reset(aac_ps_t *ps) {
    memset(ps, 0, sizeof(*ps));
}

esp_err_t aac_ps_create(aac_ps_t **out, uint32_t caps) {
    *out = audf_malloc_aligned(16, sizeof(aac_ps_t), caps);
    if (!*out) return ESP_ERR_NO_MEM;
    aac_ps_reset(*out);
    return ESP_OK;
}

void aac_ps_destroy(aac_ps_t *ps) {
    audf_free(ps);
}
