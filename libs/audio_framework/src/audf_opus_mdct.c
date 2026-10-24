/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_opus_internal.h"

#include <string.h>

#include "audf_alloc.h"

static const int s_fft_sizes[CELT_MDCT_SIZES] = { 480, 240, 120, 60 };

/* cos(pi * x) with plain double arithmetic, so the tables come out the same
 * on every target. */
static double cos_pi(double x) {
    x -= 2.0 * (double)(int64_t)(x / 2.0);
    if (x < 0) x = -x;
    if (x > 1.0) x = 2.0 - x;
    double sign = 1.0;
    if (x > 0.5) {
        x = 1.0 - x;
        sign = -1.0;
    }
    const double t = x * 3.14159265358979323846;
    const double t2 = t * t;
    double term = 1.0;
    double sum = 1.0;
    for (int k = 1; k <= 12; k++) {
        term *= -t2 / ((2 * k - 1) * (2 * k));
        sum += term;
    }
    return sign * sum;
}

static double sin_pi(double x) {
    return cos_pi(0.5 - x);
}

static int32_t q31(double v) {
    double r = v * 2147483648.0;
    r = r < 0 ? r - 0.5 : r + 0.5;
    if (r > 2147483647.0) return INT32_MAX;
    if (r < -2147483648.0) return INT32_MIN;
    return (int32_t)r;
}

static void factorize(opus_fft_t *f, int n) {
    int count = 0;
    while (n > 1) {
        int p = n % 4 == 0 ? 4 : n % 2 == 0 ? 2 : n % 3 == 0 ? 3 : 5;
        f->radix[count++] = (uint8_t)p;
        n /= p;
    }
    f->stages = (uint8_t)count;
}

static esp_err_t fft_init(opus_fft_t *f, int n, uint32_t caps) {
    f->n = (uint16_t)n;
    factorize(f, n);
    f->tw = audf_malloc((size_t)n * 2 * sizeof(int32_t), caps);
    f->perm = audf_malloc((size_t)n * sizeof(uint16_t), caps);
    if (!f->tw || !f->perm) return ESP_ERR_NO_MEM;
    for (int k = 0; k < n; k++) {
        f->tw[2 * k] = q31(cos_pi(2.0 * k / n));
        f->tw[2 * k + 1] = q31(-sin_pi(2.0 * k / n));
    }
    for (int pos = 0; pos < n; pos++) {
        int rest = pos;
        int len = n;
        int stride = 1;
        int index = 0;
        for (int s = 0; s < f->stages; s++) {
            const int p = f->radix[s];
            len /= p;
            const int digit = rest / len;
            rest %= len;
            index += digit * stride;
            stride *= p;
        }
        f->perm[pos] = (uint16_t)index;
    }
    return ESP_OK;
}

static inline int32_t mulq31(int32_t a, int32_t c) {
    return opus_mulh(a, c) * 2;
}

static inline void cmul(int32_t *re, int32_t *im, int32_t ar, int32_t ai, int32_t wr, int32_t wi) {
    *re = (opus_mulh(ar, wr) - opus_mulh(ai, wi)) * 2;
    *im = (opus_mulh(ar, wi) + opus_mulh(ai, wr)) * 2;
}

static inline void twiddle(int32_t *re, int32_t *im, const int32_t *v, const int32_t *w, int k) {
    if (k == 0) {
        *re = v[0];
        *im = v[1];
    } else {
        cmul(re, im, v[0], v[1], w[0], w[1]);
    }
}

static void stage2(int32_t *x, int n, int m, const int32_t *tw, int step) {
    for (int k = 0; k < m; k++) {
        const int32_t *w1 = tw + 2 * k * step;
        for (int g = k; g < n; g += 2 * m) {
            int32_t *o0 = x + 2 * g, *o1 = o0 + 2 * m;
            int32_t r1, i1;
            twiddle(&r1, &i1, o1, w1, k);
            const int32_t r0 = o0[0], i0 = o0[1];
            o0[0] = r0 + r1;
            o0[1] = i0 + i1;
            o1[0] = r0 - r1;
            o1[1] = i0 - i1;
        }
    }
}

static void stage3(int32_t *x, int n, int m, const int32_t *tw, int step) {
    /* sin(2pi/3) in Q31; cos(2pi/3) is -1/2 */
    const int32_t s3 = 1859775393;
    for (int k = 0; k < m; k++) {
        const int32_t *w1 = tw + 2 * k * step, *w2 = tw + 4 * k * step;
        for (int g = k; g < n; g += 3 * m) {
            int32_t *o0 = x + 2 * g, *o1 = o0 + 2 * m, *o2 = o1 + 2 * m;
            int32_t r1, i1, r2, i2;
            twiddle(&r1, &i1, o1, w1, k);
            twiddle(&r2, &i2, o2, w2, k);
            const int32_t r0 = o0[0], i0 = o0[1];
            const int32_t sr = r1 + r2, si = i1 + i2;
            const int32_t dr = r1 - r2, di = i1 - i2;
            const int32_t mr = r0 - (sr >> 1), mi = i0 - (si >> 1);
            const int32_t tr = mulq31(di, s3), ti = mulq31(dr, s3);
            o0[0] = r0 + sr;
            o0[1] = i0 + si;
            o1[0] = mr + tr;
            o1[1] = mi - ti;
            o2[0] = mr - tr;
            o2[1] = mi + ti;
        }
    }
}

static void stage4(int32_t *x, int n, int m, const int32_t *tw, int step) {
    for (int k = 0; k < m; k++) {
        const int32_t *w1 = tw + 2 * k * step, *w2 = tw + 4 * k * step, *w3 = tw + 6 * k * step;
        for (int g = k; g < n; g += 4 * m) {
            int32_t *o0 = x + 2 * g, *o1 = o0 + 2 * m, *o2 = o1 + 2 * m, *o3 = o2 + 2 * m;
            int32_t r1, i1, r2, i2, r3, i3;
            twiddle(&r1, &i1, o1, w1, k);
            twiddle(&r2, &i2, o2, w2, k);
            twiddle(&r3, &i3, o3, w3, k);
            const int32_t r0 = o0[0], i0 = o0[1];
            const int32_t ar = r0 + r2, ai = i0 + i2;
            const int32_t br = r0 - r2, bi = i0 - i2;
            const int32_t cr = r1 + r3, ci = i1 + i3;
            const int32_t dr = r1 - r3, di = i1 - i3;
            o0[0] = ar + cr;
            o0[1] = ai + ci;
            o2[0] = ar - cr;
            o2[1] = ai - ci;
            o1[0] = br + di;
            o1[1] = bi - dr;
            o3[0] = br - di;
            o3[1] = bi + dr;
        }
    }
}

static void stage5(int32_t *x, int n, int m, const int32_t *tw, int step) {
    /* cos and sin of 2pi/5 and 4pi/5, Q31 */
    const int32_t c1 = 663608942, s1 = 2042378317, c2 = -1737350766, s2 = 1262259218;
    for (int k = 0; k < m; k++) {
        const int32_t *w1 = tw + 2 * k * step, *w2 = tw + 4 * k * step;
        const int32_t *w3 = tw + 6 * k * step, *w4 = tw + 8 * k * step;
        for (int g = k; g < n; g += 5 * m) {
            int32_t *o0 = x + 2 * g, *o1 = o0 + 2 * m, *o2 = o1 + 2 * m, *o3 = o2 + 2 * m, *o4 = o3 + 2 * m;
            int32_t r1, i1, r2, i2, r3, i3, r4, i4;
            twiddle(&r1, &i1, o1, w1, k);
            twiddle(&r2, &i2, o2, w2, k);
            twiddle(&r3, &i3, o3, w3, k);
            twiddle(&r4, &i4, o4, w4, k);
            const int32_t r0 = o0[0], i0 = o0[1];
            const int32_t ar = r1 + r4, ai = i1 + i4;
            const int32_t br = r1 - r4, bi = i1 - i4;
            const int32_t cr = r2 + r3, ci = i2 + i3;
            const int32_t dr = r2 - r3, di = i2 - i3;
            const int32_t m1r = r0 + mulq31(ar, c1) + mulq31(cr, c2);
            const int32_t m1i = i0 + mulq31(ai, c1) + mulq31(ci, c2);
            const int32_t m2r = r0 + mulq31(ar, c2) + mulq31(cr, c1);
            const int32_t m2i = i0 + mulq31(ai, c2) + mulq31(ci, c1);
            const int32_t n1r = mulq31(bi, s1) + mulq31(di, s2);
            const int32_t n1i = mulq31(br, s1) + mulq31(dr, s2);
            const int32_t n2r = mulq31(bi, s2) - mulq31(di, s1);
            const int32_t n2i = mulq31(br, s2) - mulq31(dr, s1);
            o0[0] = r0 + ar + cr;
            o0[1] = i0 + ai + ci;
            o1[0] = m1r + n1r;
            o1[1] = m1i - n1i;
            o4[0] = m1r - n1r;
            o4[1] = m1i + n1i;
            o2[0] = m2r + n2r;
            o2[1] = m2i - n2i;
            o3[0] = m2r - n2r;
            o3[1] = m2i + n2i;
        }
    }
}

/* Forward complex FFT in place on interleaved re/im, input already permuted. */
static void fft_stages(const opus_fft_t *f, int32_t *x) {
    const int n = f->n;
    int m = 1;
    for (int s = f->stages - 1; s >= 0; s--) {
        const int p = f->radix[s];
        const int step = n / (p * m);
        switch (p) {
        case 2: stage2(x, n, m, f->tw, step); break;
        case 3: stage3(x, n, m, f->tw, step); break;
        case 4: stage4(x, n, m, f->tw, step); break;
        default: stage5(x, n, m, f->tw, step); break;
        }
        m *= p;
    }
}

esp_err_t celt_mdct_init(celt_mdct_t *mdct, uint32_t caps) {
    memset(mdct, 0, sizeof(*mdct));
    for (int s = 0; s < CELT_MDCT_SIZES; s++) {
        const int n4 = s_fft_sizes[s];
        const int n2 = 2 * n4;
        esp_err_t err = fft_init(&mdct->fft[s], n4, caps);
        if (err != ESP_OK) return err;
        mdct->rot[s] = audf_malloc((size_t)n4 * 2 * sizeof(int32_t), caps);
        if (!mdct->rot[s]) return ESP_ERR_NO_MEM;
        for (int k = 0; k < n4; k++) {
            const double a = (k + 0.125) / n2;
            mdct->rot[s][2 * k] = q31(cos_pi(a));
            mdct->rot[s][2 * k + 1] = q31(-sin_pi(a));
        }
    }
    mdct->buf = audf_malloc(2 * 960 * sizeof(int32_t), caps);
    if (!mdct->buf) return ESP_ERR_NO_MEM;
    return ESP_OK;
}

void celt_mdct_free(celt_mdct_t *mdct) {
    for (int s = 0; s < CELT_MDCT_SIZES; s++) {
        audf_free(mdct->fft[s].tw);
        audf_free(mdct->fft[s].perm);
        audf_free(mdct->rot[s]);
    }
    audf_free(mdct->buf);
}

void celt_imdct(const celt_mdct_t *mdct, const int32_t *in, int stride, int shift, int32_t *out) {
    const opus_fft_t *f = &mdct->fft[shift];
    const int32_t *rot = mdct->rot[shift];
    const int n4 = f->n;
    const int n2 = 2 * n4;
    int32_t *z = mdct->buf;
    int32_t *u = mdct->buf + n2;

    uint32_t l1 = 0;
    for (int k = 0; k < n2; k++) {
        const int32_t v = in[k * stride];
        const uint32_t a = v < 0 ? (uint32_t)-(int64_t)v : (uint32_t)v;
        l1 = l1 + a < l1 ? UINT32_MAX : l1 + a;
    }
    const int down = l1 >= (1u << 30) ? opus_ilog(l1) - 30 : 0;

    for (int pos = 0; pos < n4; pos++) {
        const int k = f->perm[pos];
        const int32_t re = in[2 * k * stride] >> down;
        const int32_t im = in[(n2 - 1 - 2 * k) * stride] >> down;
        cmul(&z[2 * pos], &z[2 * pos + 1], re, im, rot[2 * k], rot[2 * k + 1]);
    }
    fft_stages(f, z);
    for (int k = 0; k < n4; k++) {
        int32_t re, im;
        cmul(&re, &im, z[2 * k], z[2 * k + 1], rot[2 * k], rot[2 * k + 1]);
        u[2 * k] = re;
        u[n2 - 1 - 2 * k] = -im;
    }

    /* y[n] of the 2 * n2 output, windowed and written from n = (n2 - overlap) / 2 */
    const int lead = (n2 - CELT_OVERLAP) / 2;
    const int half = n2 / 2;
    for (int j = 0; j < n2 + CELT_OVERLAP; j++) {
        const int n = j + lead;
        int32_t y;
        if (n < half) {
            y = u[n + half];
        } else if (n < 3 * half) {
            y = -u[3 * half - 1 - n];
        } else {
            y = -u[n - 3 * half];
        }
        if (down) y = celt_sat_sig((int64_t)y << down);
        if (j < CELT_OVERLAP) {
            out[j] += mulq31(y, celt_window[j] << 16);
        } else if (j < n2) {
            out[j] = y;
        } else {
            out[j] = mulq31(y, celt_window[n2 + CELT_OVERLAP - 1 - j] << 16);
        }
    }
}
