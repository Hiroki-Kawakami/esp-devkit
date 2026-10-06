/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <string.h>

#include "audf_aac.h"
#include "audf_alloc.h"
#include "audf_aac_kernels.h"
#include "audf_aac_tables.h"

#if AAC_HAVE_PIE
static uint32_t next(uint32_t *s) {
    *s = *s * 1664525u + 1013904223u;
    return *s;
}
#endif

static inline void quarter(int32_t *p, int32_t r, int32_t i) {
    p[0] = r >> 2;
    p[1] = i >> 2;
}

static inline void rotate_quarter(int32_t *p, int32_t r, int32_t i, const int32_t *w) {
    p[0] = (aac_mulh(r, w[0]) - aac_mulh(i, w[1])) >> 1;
    p[1] = (aac_mulh(r, w[1]) + aac_mulh(i, w[0])) >> 1;
}

static void fft64(int32_t *z) {
    int len = 64;
    while (len >= 4) {
        const int q = len >> 2;
        const int step = 2 * (64 / len);
        for (int b = 0; b < 64; b += len) {
            int32_t *x0 = z + 2 * b;
            for (int j = 0; j < q; j++, x0 += 2) {
                int32_t *x1 = x0 + 2 * q, *x2 = x1 + 2 * q, *x3 = x2 + 2 * q;
                const int32_t ar = x0[0] + x2[0], ai = x0[1] + x2[1];
                const int32_t br = x0[0] - x2[0], bi = x0[1] - x2[1];
                const int32_t cr = x1[0] + x3[0], ci = x1[1] + x3[1];
                const int32_t dr = x1[0] - x3[0], di = x1[1] - x3[1];
                quarter(x0, ar + cr, ai + ci);
                if (j) {
                    rotate_quarter(x1, br + di, bi - dr, aac_fft_twiddle31 + j * step);
                    rotate_quarter(x2, ar - cr, ai - ci, aac_fft_twiddle31 + 2 * j * step);
                    rotate_quarter(x3, br - di, bi + dr, aac_fft_twiddle31 + 3 * j * step);
                } else {
                    quarter(x1, br + di, bi - dr);
                    quarter(x2, ar - cr, ai - ci);
                    quarter(x3, br - di, bi + dr);
                }
            }
        }
        len = q;
    }
}

void aac_k_dct4_128(int32_t *x, int32_t *tmp) {
    for (int n = 0; n < 64; n++) {
        const int32_t vr = x[2 * n], vi = x[127 - 2 * n];
        const int32_t wr = aac_dct4_pre31_128[2 * n], wi = aac_dct4_pre31_128[2 * n + 1];
        tmp[2 * n] = aac_mulh(vr, wr) - aac_mulh(vi, wi);
        tmp[2 * n + 1] = aac_mulh(vr, wi) + aac_mulh(vi, wr);
    }
    fft64(tmp);
    x[0] = tmp[0] >> 1;
    x[127] = -(tmp[1] >> 1);
    for (int k = 1; k < 64; k++) {
        const int32_t *t = tmp + 2 * aac_fft_pos_64[k];
        const int32_t wr = aac_dct4_post31_128[2 * k], wi = aac_dct4_post31_128[2 * k + 1];
        x[2 * k] = aac_mulh(t[0], wr) - aac_mulh(t[1], wi);
        x[127 - 2 * k] = -(aac_mulh(t[0], wi) + aac_mulh(t[1], wr));
    }
}

static inline int32_t split_mul(int16_t hi, int16_t lo, int16_t c) {
    return hi * c + ((lo * c) >> 16);
}

void aac_k_qmf_analysis_c(int32_t *u, const int16_t *hi, const int16_t *lo, const int16_t *c) {
    for (int p = 0; p < 64; p++) u[p] = 0;
    for (int j = 0; j < 5; j++) {
        const int16_t *h = hi - 64 * j, *l = lo - 64 * j, *cj = c + 64 * j;
        for (int p = 0; p < 64; p++) u[p] += split_mul(h[p], l[p], cj[p]);
    }
}

void aac_k_qmf_synthesis_c(int32_t *acc, const int16_t *hi, const int16_t *lo, const int16_t *c) {
    for (int k = 0; k < 64; k++) acc[k] = 0;
    for (int t = 0; t < 10; t++) {
        const int off = (t >> 1) * 256 + (t & 1) * 192;
        const int16_t *h = hi + off, *l = lo + off, *ct = c + 64 * t;
        for (int k = 0; k < 64; k++) acc[k] += split_mul(h[k], l[k], ct[k]);
    }
}

static inline void cmul15(int32_t ar, int32_t ai, const int16_t *w, int32_t *re, int32_t *im) {
    *re = aac_mulh16(ar, w[0]) - aac_mulh16(ai, w[1]);
    *im = aac_mulh16(ar, w[1]) + aac_mulh16(ai, w[0]);
}

static inline int bitrev5(int k) {
    return (k & 1) << 4 | (k & 2) << 2 | (k & 4) | (k & 8) >> 2 | (k & 16) >> 4;
}

void aac_k_dct4_64x4_c(int32_t *x, int32_t *tmp) {
    int32_t *re = tmp, *im = tmp + 128;
    for (int l = 0; l < 4; l++) {
        for (int n = 0; n < 32; n++) {
            cmul15(x[(2 * n) * 4 + l], x[(63 - 2 * n) * 4 + l], aac_q15_dct64_pre + 2 * n, &re[n * 4 + l],
                   &im[n * 4 + l]);
        }
        for (int len = 32; len >= 2; len >>= 1) {
            const int h = len >> 1, step = 32 / len;
            for (int b = 0; b < 32; b += len) {
                for (int j = 0; j < h; j++) {
                    const int a = (b + j) * 4 + l, c = (b + j + h) * 4 + l;
                    const int32_t ar = re[a], ai = im[a], cr = re[c], ci = im[c];
                    re[a] = (ar + cr) >> 1;
                    im[a] = (ai + ci) >> 1;
                    const int32_t dr = ar - cr, di = ai - ci;
                    if (!j) {
                        re[c] = dr >> 1;
                        im[c] = di >> 1;
                    } else if (len == 4) {
                        re[c] = di >> 1;
                        im[c] = -dr >> 1;
                    } else {
                        cmul15(dr, di, aac_q15_fft32 + 2 * j * step, &re[c], &im[c]);
                    }
                }
            }
        }
        for (int k = 0; k < 32; k++) {
            const int p = bitrev5(k) * 4 + l;
            int32_t ur, ui;
            if (k) {
                cmul15(re[p], im[p], aac_q15_dct64_post + 2 * k, &ur, &ui);
            } else {
                ur = re[p] >> 1;
                ui = im[p] >> 1;
            }
            x[(2 * k) * 4 + l] = ur;
            x[(63 - 2 * k) * 4 + l] = -ui;
        }
    }
}

static inline void cmul_lane(int32_t ar, int32_t ai, const int16_t *t, int idx, int32_t *re, int32_t *im) {
    const int16_t w[2] = { t[8 * (idx >> 2) + (idx & 3)], t[8 * (idx >> 2) + 4 + (idx & 3)] };
    cmul15(ar, ai, w, re, im);
}

void aac_k_dct4_1024_c(int32_t *x, int32_t *tmp) {
    int32_t *re = tmp, *im = tmp + 512;
    for (int n = 0; n < 512; n++) cmul_lane(x[2 * n], x[1023 - 2 * n], aac_pie_dct1024_pre, n, &re[n], &im[n]);
    for (int l = 0; l < 4; l++) {
        for (int len = 128; len >= 2; len >>= 1) {
            const int h = len >> 1, step = 128 / len;
            for (int b = 0; b < 128; b += len) {
                for (int j = 0; j < h; j++) {
                    const int a = (b + j) * 4 + l, c = (b + j + h) * 4 + l;
                    const int32_t ar = re[a], ai = im[a], cr = re[c], ci = im[c];
                    re[a] = (ar + cr) >> 1;
                    im[a] = (ai + ci) >> 1;
                    const int32_t dr = ar - cr, di = ai - ci;
                    if (!j) {
                        re[c] = dr >> 1;
                        im[c] = di >> 1;
                    } else if (len == 4) {
                        re[c] = di >> 1;
                        im[c] = -dr >> 1;
                    } else {
                        cmul_lane(dr, di, aac_pie_dct1024_fft128, 4 * j * step, &re[c], &im[c]);
                    }
                }
            }
        }
    }
    static const uint8_t slot_of[4] = { 0, 64, 32, 96 };
    for (int p = 0; p < 32; p++) {
        for (int i = 0; i < 4; i++) {
            const int at = (p + slot_of[i]) * 4;
            int32_t gr[4], gi[4];
            gr[0] = re[at] >> 1;
            gi[0] = im[at] >> 1;
            for (int l = 1; l < 4; l++) {
                cmul_lane(re[at + l], im[at + l], aac_pie_dct1024_mid, 12 * p + 4 * (l - 1) + i, &gr[l], &gi[l]);
            }
            const int32_t ar = gr[0] + gr[2], ai = gi[0] + gi[2];
            const int32_t br = gr[0] - gr[2], bi = gi[0] - gi[2];
            const int32_t cr = gr[1] + gr[3], ci = gi[1] + gi[3];
            const int32_t dr = gi[1] - gi[3], di = gr[3] - gr[1];
            const int32_t zr[4] = { (ar + cr) >> 1, (br + dr) >> 1, (ar - cr) >> 1, (br - dr) >> 1 };
            const int32_t zi[4] = { (ai + ci) >> 1, (bi + di) >> 1, (ai - ci) >> 1, (bi - di) >> 1 };
            for (int q = 0; q < 4; q++) {
                const int k = (bitrev5(p) << 2) + i + 128 * q;
                int32_t ur, ui;
                if (k) {
                    cmul_lane(zr[q], zi[q], aac_pie_dct1024_post, 4 * (p + 32 * q) + i, &ur, &ui);
                } else {
                    ur = zr[q] >> 1;
                    ui = zi[q] >> 1;
                }
                x[k] = ur;
                x[512 + k] = -ui;
            }
        }
    }
}

static inline int32_t wadd(int32_t a, int32_t b) {
    return (int32_t)((uint32_t)a + (uint32_t)b);
}

static inline int32_t wsub(int32_t a, int32_t b) {
    return (int32_t)((uint32_t)a - (uint32_t)b);
}

static inline int32_t wshl(int32_t a, int n) {
    return (int32_t)((uint32_t)a << n);
}

static inline void cmul_vec(int32_t xr, int32_t xi, const int16_t *v, int l, int32_t *re, int32_t *im) {
    *re = wsub(aac_mulh16(xr, v[l]), aac_mulh16(xi, v[4 + l]));
    *im = wadd(aac_mulh16(xr, v[4 + l]), aac_mulh16(xi, v[l]));
}

static inline int lane(int k, int c) {
    return (k >> 2) * 8 + c * 4 + (k & 3);
}

void aac_k_ps_allpass_c(int32_t *r, const int32_t *s, int32_t *ap, int w) {
    for (int k = 0; k < 32; k++) {
        const int l = k & 3, re = lane(k, 0), im = lane(k, 1);
        const int16_t *t = aac_pie_ps_ap + 56 * (k >> 2);
        int32_t ir, ii;
        cmul_vec(s[re], s[im], t, l, &ir, &ii);
        ir = wshl(ir, 1);
        ii = wshl(ii, 1);
        for (int m = 0; m < 3; m++) {
            const int16_t *q = t + 8 + 16 * m;
            const int32_t *link = ap + (m * 8 + ((w - 3 - m) & 7)) * 64;
            int32_t cr, ci;
            cmul_vec(link[re], link[im], q, l, &cr, &ci);
            const int32_t tr = wshl(wsub(cr, aac_mulh16(ir, q[8 + l])), 1);
            const int32_t ti = wshl(wsub(ci, aac_mulh16(ii, q[12 + l])), 1);
            int32_t *dst = ap + (m * 8 + w) * 64;
            dst[re] = wadd(ir, wshl(aac_mulh16(tr, q[8 + l]), 1));
            dst[im] = wadd(ii, wshl(aac_mulh16(ti, q[12 + l]), 1));
            ir = tr;
            ii = ti;
        }
        r[re] = ir;
        r[im] = ii;
    }
}

void aac_k_ps_gain_c(int32_t *r, const int16_t *g, int groups) {
    for (int k = 0; k < 4 * groups; k++) {
        const int16_t *v = g + 8 * (k >> 2);
        r[lane(k, 0)] = wshl(aac_mulh16(r[lane(k, 0)], v[k & 3]), 1);
        r[lane(k, 1)] = wshl(aac_mulh16(r[lane(k, 1)], v[4 + (k & 3)]), 1);
    }
}

void aac_k_ps_mix_c(int32_t *l, int32_t *r, int32_t *acc, const int32_t *step, int groups) {
    for (int g = 0; g < groups; g++) {
        int16_t c[4][8];
        for (int j = 0; j < 4; j++) {
            for (int n = 0; n < 8; n++) {
                int32_t *a = acc + 32 * g + 8 * j + n;
                *a = wadd(*a, step[32 * g + 8 * j + n]);
                c[j][n] = (int16_t)(wadd(*a, 0x8000) >> 16);
            }
        }
        for (int n = 0; n < 4; n++) {
            const int k = 4 * g + n, re = lane(k, 0), im = lane(k, 1);
            int32_t ar, ai, br, bi, cr, ci, dr, di;
            cmul_vec(l[re], l[im], c[0], n, &ar, &ai);
            cmul_vec(r[re], r[im], c[1], n, &br, &bi);
            cmul_vec(l[re], l[im], c[2], n, &cr, &ci);
            cmul_vec(r[re], r[im], c[3], n, &dr, &di);
            l[re] = wshl(wadd(ar, br), 2);
            l[im] = wshl(wadd(ai, bi), 2);
            r[re] = wshl(wadd(cr, dr), 2);
            r[im] = wshl(wadd(ci, di), 2);
        }
    }
}

void aac_k_ps_mix_real_c(int32_t *l, int32_t *r, int32_t *acc, const int32_t *step, int groups) {
    for (int g = 0; g < groups; g++) {
        int16_t c[4][4];
        for (int j = 0; j < 4; j++) {
            for (int n = 0; n < 4; n++) {
                int32_t *a = acc + 32 * g + 8 * j + n;
                *a = wadd(*a, step[32 * g + 8 * j + n]);
                c[j][n] = (int16_t)(wadd(*a, 0x8000) >> 16);
            }
        }
        for (int n = 0; n < 4; n++) {
            for (int p = 0; p < 2; p++) {
                const int i = 8 * g + 4 * p + n;
                const int32_t x = l[i], y = r[i];
                l[i] = wshl(wadd(aac_mulh16(x, c[0][n]), aac_mulh16(y, c[1][n])), 2);
                r[i] = wshl(wadd(aac_mulh16(x, c[2][n]), aac_mulh16(y, c[3][n])), 2);
            }
        }
    }
}

void aac_k_ps_unzip_c(int32_t *dst, const int32_t *src, int groups) {
    for (int g = 0; g < groups; g++, dst += 8, src += 8) {
        for (int n = 0; n < 4; n++) {
            dst[n] = src[2 * n];
            dst[4 + n] = src[2 * n + 1];
        }
    }
}

void aac_k_ps_zip_c(int32_t *dst, const int32_t *src, int groups) {
    for (int g = 0; g < groups; g++, dst += 8, src += 8) {
        for (int n = 0; n < 4; n++) {
            dst[2 * n] = src[n];
            dst[2 * n + 1] = src[4 + n];
        }
    }
}

void aac_k_syn_in4_c(int32_t *a, int32_t *b, const int32_t *const x[4]) {
    for (int l = 0; l < 4; l++) {
        for (int k = 0; k < 64; k++) {
            a[k * 4 + l] = x[l][2 * k] * 16;
            b[(63 - k) * 4 + l] = x[l][2 * k + 1] * 16;
        }
    }
}

void aac_k_syn_v4_c(int16_t *hi, int16_t *lo, const int32_t *a, const int32_t *b) {
    for (int l = 0; l < 4; l++) {
        int16_t *h = hi - 128 * (l + 1), *o = lo - 128 * (l + 1);
        for (int n = 0; n < 64; n++) {
            const int32_t an = a[n * 4 + l];
            const int32_t bn = (n & 1) ? -b[n * 4 + l] : b[n * 4 + l];
            aac_split(h + n, o + n, (bn - an) * 2);
            aac_split(h + 127 - n, o + 127 - n, (an + bn) * 2);
        }
    }
}

static inline int16_t pack_one(int32_t v, int shift) {
    v = (v + (1 << (shift - 1))) >> shift;
    if (v > INT16_MAX) return INT16_MAX;
    if (v < INT16_MIN) return INT16_MIN;
    return (int16_t)v;
}

void aac_k_pack_c(int16_t *out, const int32_t *a, const int32_t *b, int n, int shift) {
    if (!b) {
        for (int i = 0; i < n; i++) out[i] = pack_one(a[i], shift);
        return;
    }
    for (int i = 0; i < n; i++) {
        out[2 * i] = pack_one(a[i], shift);
        out[2 * i + 1] = pack_one(b[i], shift);
    }
}

int audf_aac_kernel_selftest(uint32_t seed, int iterations) {
#if AAC_HAVE_PIE
    struct {
        int16_t hi[2560], lo[2560];
        int32_t a[64], ra[64];
        int16_t pa[128], pb[128];
        int32_t d1[256], d2[256], w[256], rows[512];
        int32_t ia[256], ib[256], ja[256], jb[256];
        int16_t vh[2][640], vl[2][640];
        int32_t l1[1024], l2[1024], lt[1024];
        int32_t xa[2][2 * 72], xb[2][2 * 72], ps[64], pap[2][3 * 8 * 64];
        int16_t pg[18 * 8];
        int32_t pacc[2][18 * 32], pstep[18 * 32];
    } *t = audf_malloc_aligned(16, sizeof(*t), 0);
    if (!t) return -1;
    int bad = 0;
    aac_k_prepare();
    for (int it = 0; it < iterations; it++) {
        const int bits = 8 + (int)(next(&seed) % 18);
        for (int i = 0; i < 2560; i++) {
            const int32_t v = (int32_t)next(&seed) >> (31 - bits);
            aac_split(t->hi + i, t->lo + i, v);
        }
        aac_k_qmf_synthesis(t->a, t->hi, t->lo, sbr_qmf_c);
        aac_k_qmf_synthesis_c(t->ra, t->hi, t->lo, sbr_qmf_c);
        bad += memcmp(t->a, t->ra, sizeof(t->a)) != 0;
        aac_k_qmf_analysis(t->a, t->hi + 512, t->lo + 512, sbr_qmf_c2r);
        aac_k_qmf_analysis_c(t->ra, t->hi + 512, t->lo + 512, sbr_qmf_c2r);
        bad += memcmp(t->a, t->ra, sizeof(t->a)) != 0;
        for (int i = 0; i < 64; i++) {
            t->a[i] = (int32_t)next(&seed) >> (31 - bits - 4);
            t->ra[i] = (int32_t)next(&seed) >> (31 - bits - 4);
        }
        const int shift = 1 + (int)(next(&seed) % 10);
        aac_k_pack(t->pa, t->a, t->ra, 64, shift);
        aac_k_pack_c(t->pb, t->a, t->ra, 64, shift);
        bad += memcmp(t->pa, t->pb, sizeof(t->pa)) != 0;
        aac_k_pack(t->pa, t->a, NULL, 64, shift);
        aac_k_pack_c(t->pb, t->a, NULL, 64, shift);
        bad += memcmp(t->pa, t->pb, 64 * sizeof(int16_t)) != 0;
        for (int i = 0; i < 256; i++) t->d1[i] = t->d2[i] = (int32_t)next(&seed) >> (31 - bits - 3);
        aac_k_dct4_64x4(t->d1, t->w);
        aac_k_dct4_64x4_c(t->d2, t->w);
        bad += memcmp(t->d1, t->d2, sizeof(t->d1)) != 0;
        for (int i = 0; i < 512; i++) t->rows[i] = (int32_t)next(&seed) >> (31 - bits);
        const int32_t *const rp[4] = { t->rows, t->rows + 128, t->rows + 256, t->rows + 384 };
        aac_k_syn_in4(t->ia, t->ib, rp);
        aac_k_syn_in4_c(t->ja, t->jb, rp);
        bad += memcmp(t->ia, t->ja, sizeof(t->ia)) != 0 || memcmp(t->ib, t->jb, sizeof(t->ib)) != 0;
        memset(t->vh, 0, sizeof(t->vh));
        memset(t->vl, 0, sizeof(t->vl));
        aac_k_syn_v4(t->vh[0] + 576, t->vl[0] + 576, t->d1, t->ia);
        aac_k_syn_v4_c(t->vh[1] + 576, t->vl[1] + 576, t->d1, t->ia);
        bad += memcmp(t->vh[0], t->vh[1], sizeof(t->vh[0])) != 0 || memcmp(t->vl[0], t->vl[1], sizeof(t->vl[0])) != 0;
        for (int i = 0; i < 1024; i++) t->l1[i] = t->l2[i] = (int32_t)next(&seed) >> (31 - bits - 4);
        aac_k_dct4_1024(t->l1, t->lt);
        aac_k_dct4_1024_c(t->l2, t->lt);
        bad += memcmp(t->l1, t->l2, sizeof(t->l1)) != 0;
        for (int i = 0; i < 2 * 72; i++) {
            t->xa[0][i] = t->xa[1][i] = (int32_t)next(&seed) >> (31 - bits - 2);
            t->xb[0][i] = t->xb[1][i] = (int32_t)next(&seed) >> (31 - bits - 2);
        }
        for (int i = 0; i < 64; i++) t->ps[i] = (int32_t)next(&seed) >> (31 - bits - 2);
        for (int i = 0; i < 3 * 8 * 64; i++) t->pap[0][i] = t->pap[1][i] = (int32_t)next(&seed) >> (31 - bits - 2);
        for (int i = 0; i < 18 * 8; i++) t->pg[i] = (int16_t)next(&seed);
        for (int i = 0; i < 18 * 32; i++) {
            t->pacc[0][i] = t->pacc[1][i] = (int32_t)next(&seed) >> 1;
            t->pstep[i] = (int32_t)next(&seed) >> 6;
        }
        const int w = (int)(next(&seed) & 7);
        aac_k_ps_allpass(t->xa[0], t->ps, t->pap[0], w);
        aac_k_ps_allpass_c(t->xa[1], t->ps, t->pap[1], w);
        bad += memcmp(t->xa[0], t->xa[1], sizeof(t->xa[0])) != 0;
        bad += memcmp(t->pap[0], t->pap[1], sizeof(t->pap[0])) != 0;
        aac_k_ps_gain(t->xa[0], t->pg, 18);
        aac_k_ps_gain_c(t->xa[1], t->pg, 18);
        bad += memcmp(t->xa[0], t->xa[1], sizeof(t->xa[0])) != 0;
        aac_k_ps_mix(t->xa[0], t->xb[0], t->pacc[0], t->pstep, 18);
        aac_k_ps_mix_c(t->xa[1], t->xb[1], t->pacc[1], t->pstep, 18);
        bad += memcmp(t->pacc[0], t->pacc[1], sizeof(t->pacc[0])) != 0;
        aac_k_ps_mix_real(t->xa[0], t->xb[0], t->pacc[0], t->pstep, 18);
        aac_k_ps_mix_real_c(t->xa[1], t->xb[1], t->pacc[1], t->pstep, 18);
        bad += memcmp(t->xa[0], t->xa[1], sizeof(t->xa[0])) != 0 || memcmp(t->xb[0], t->xb[1], sizeof(t->xb[0])) != 0;
        bad += memcmp(t->pacc[0], t->pacc[1], sizeof(t->pacc[0])) != 0;
        aac_k_ps_unzip(t->xb[0], t->xa[0], 18);
        aac_k_ps_unzip_c(t->xb[1], t->xa[0], 18);
        bad += memcmp(t->xb[0], t->xb[1], sizeof(t->xb[0])) != 0;
        aac_k_ps_zip(t->xb[0], t->xa[0], 18);
        aac_k_ps_zip_c(t->xb[1], t->xa[0], 18);
        bad += memcmp(t->xb[0], t->xb[1], sizeof(t->xb[0])) != 0;
        bad += memcmp(t->xa[0], t->xa[1], sizeof(t->xa[0])) != 0 || memcmp(t->xb[0], t->xb[1], sizeof(t->xb[0])) != 0;
    }
    audf_free(t);
    return bad;
#else
    (void)seed;
    (void)iterations;
    return 0;
#endif
}
