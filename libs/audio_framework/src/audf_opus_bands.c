/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_opus_internal.h"

#include <math.h>
#include <string.h>

#include "audf_alloc.h"

#define BITRES 3
#define QTHETA_OFFSET 4
#define QTHETA_OFFSET_TWOPHASE 16
#define SPREAD_NONE 0
#define SPREAD_AGGRESSIVE 3

uint32_t celt_lcg(uint32_t seed) {
    return 1664525u * seed + 1013904223u;
}

float opus_exp2(float x) {
    const float i = floorf(x);
    const float f = (x - i) * 0.69314718056f;
    float term = 1.0f;
    float sum = 1.0f;
    for (int k = 1; k <= 8; k++) {
        term = term * f / (float)k;
        sum += term;
    }
    return ldexpf(sum, (int)i);
}

float opus_cos_half_pi(float x) {
    const float t = x * 1.57079632679f;
    const float t2 = t * t;
    float term = 1.0f;
    float sum = 1.0f;
    for (int k = 1; k <= 7; k++) {
        term = -term * t2 / (float)((2 * k - 1) * (2 * k));
        sum += term;
    }
    return sum;
}

static inline int32_t mul_q15(int32_t a, int32_t c) {
    return (int32_t)(((int64_t)a * c + 16384) >> 15);
}

static inline int frac_mul16(int a, int b) {
    return (16384 + (int32_t)(int16_t)a * (int16_t)b) >> 15;
}

static int bitexact_cos(int x) {
    const int x2 = (4096 + x * x) >> 13;
    const int r = (32767 - x2) + frac_mul16(x2, -7651 + frac_mul16(x2, 8277 + frac_mul16(-626, x2)));
    return 1 + r;
}

static int bitexact_log2tan(int isin, int icos) {
    const int lc = opus_ilog((uint32_t)icos);
    const int ls = opus_ilog((uint32_t)isin);
    icos <<= 15 - lc;
    isin <<= 15 - ls;
    return (ls - lc) * (1 << 11) + frac_mul16(isin, frac_mul16(isin, -2597) + 7932) -
           frac_mul16(icos, frac_mul16(icos, -2597) + 7932);
}

static uint32_t isqrt32(uint32_t v) {
    uint32_t g = 0;
    int shift = (opus_ilog(v) - 1) >> 1;
    uint32_t b = 1u << shift;
    do {
        const uint32_t t = ((g << 1) + b) << shift;
        if (t <= v) {
            g += b;
            v -= t;
        }
        b >>= 1;
        shift--;
    } while (shift >= 0);
    return g;
}

static int get_pulses(int q) {
    return q < 8 ? q : (8 + (q & 7)) << ((q >> 3) - 1);
}

static const uint8_t *pulse_cache(int band, int lm) {
    return celt_cache_bits + celt_cache_index[(lm + 1) * CELT_BANDS + band];
}

int celt_bits2pulses(int band, int lm, int bits) {
    const uint8_t *cache = pulse_cache(band, lm);
    int lo = 0;
    int hi = cache[0];
    bits--;
    for (int i = 0; i < 6; i++) {
        const int mid = (lo + hi + 1) >> 1;
        if (cache[mid] >= bits) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    return bits - (lo == 0 ? -1 : cache[lo]) <= cache[hi] - bits ? lo : hi;
}

int celt_pulses2bits(int band, int lm, int pulses) {
    return pulses == 0 ? 0 : pulse_cache(band, lm)[pulses] + 1;
}

esp_err_t celt_pvq_init(celt_pvq_t *pvq, uint32_t caps) {
    uint64_t prev[129], cur[129];
    uint32_t count = 0;
    for (int n = 0; n <= CELT_MAX_PVQ_N; n++) {
        for (int k = 0; k <= 128; k++) {
            cur[k] = n == 0 ? k == 0 : k == 0 ? 1 : prev[k] + cur[k - 1] + prev[k - 1];
            if (cur[k] > UINT32_MAX) cur[k] = (uint64_t)1 << 40;
        }
        int max_k = 0;
        while (max_k < 128 && cur[max_k + 1] <= UINT32_MAX) max_k++;
        pvq->row[n] = (uint16_t)count;
        pvq->max_k[n] = (uint8_t)max_k;
        count += (uint32_t)max_k + 1;
        memcpy(prev, cur, sizeof(cur));
    }
    pvq->v = audf_malloc(count * sizeof(uint32_t), caps);
    if (!pvq->v) return ESP_ERR_NO_MEM;
    for (int n = 0; n <= CELT_MAX_PVQ_N; n++) {
        uint32_t *row = pvq->v + pvq->row[n];
        const uint32_t *up = n ? pvq->v + pvq->row[n - 1] : NULL;
        for (int k = 0; k <= pvq->max_k[n]; k++) {
            row[k] = n == 0 ? k == 0 : k == 0 ? 1 : up[k] + row[k - 1] + up[k - 1];
        }
    }
    return ESP_OK;
}

/* RFC 6716 4.3.4.2: the codeword index of K pulses in N dimensions. */
static void pvq_decode(const celt_pvq_t *pvq, opus_rc_t *rc, int *y, int n, int k) {
    uint32_t i = opus_rc_uint(rc, pvq->v[pvq->row[n] + k]);
    for (int j = 0; j < n; j++) {
        const uint32_t *low = pvq->v + pvq->row[n - j - 1];
        const uint32_t *cur = pvq->v + pvq->row[n - j];
        uint64_t p = ((uint64_t)low[k] + cur[k]) >> 1;
        int sign = 1;
        if (i >= p) {
            sign = -1;
            i -= (uint32_t)p;
        }
        const int k0 = k;
        p -= low[k];
        while (p > i) {
            k--;
            p -= low[k];
        }
        y[j] = sign * (k0 - k);
        i -= (uint32_t)p;
    }
}

/* c and s in Q31. */
static void rotate_pairs(int32_t *x, int len, int stride, int32_t c, int32_t s) {
    int32_t *p = x;
    for (int i = 0; i < len - stride; i++, p++) {
        const int32_t x1 = p[0];
        const int32_t x2 = p[stride];
        p[stride] = (opus_mulh(c, x2) + opus_mulh(s, x1)) * 2;
        p[0] = (opus_mulh(c, x1) - opus_mulh(s, x2)) * 2;
    }
    p = x + len - 2 * stride - 1;
    for (int i = len - 2 * stride - 1; i >= 0; i--, p--) {
        const int32_t x1 = p[0];
        const int32_t x2 = p[stride];
        p[stride] = (opus_mulh(c, x2) + opus_mulh(s, x1)) * 2;
        p[0] = (opus_mulh(c, x1) - opus_mulh(s, x2)) * 2;
    }
}

/* Undoes the encoder's spreading rotation (RFC 6716 4.3.4.3). */
static void unspread(int32_t *x, int len, int stride, int k, int spread) {
    static const int factor[3] = { 15, 10, 5 };
    if (2 * k >= len || spread == SPREAD_NONE) return;
    const float gain = (float)len / (float)(len + factor[spread - 1] * k);
    const float theta = 0.5f * gain * gain;
    const int32_t c = (int32_t)lrintf(opus_cos_half_pi(theta) * 2147483520.0f);
    const int32_t s = (int32_t)lrintf(opus_cos_half_pi(1.0f - theta) * 2147483520.0f);
    int stride2 = 0;
    if (len >= 8 * stride) {
        stride2 = 1;
        while ((stride2 * stride2 + stride2) * stride + (stride >> 2) < len) stride2++;
    }
    len /= stride;
    for (int i = 0; i < stride; i++) {
        if (stride2) rotate_pairs(x + i * len, len, stride2, s, c);
        rotate_pairs(x + i * len, len, 1, c, s);
    }
}

static int64_t energy(const int32_t *x, int n) {
    int64_t e = 0;
    for (int i = 0; i < n; i++) e += (int64_t)x[i] * x[i];
    return e;
}

void celt_renormalise(int32_t *x, int n, float gain) {
    const int64_t e = energy(x, n);
    if (e <= 0) return;
    const float f = gain * (float)NORM_ONE / sqrtf((float)e);
    for (int i = 0; i < n; i++) x[i] = (int32_t)lrintf((float)x[i] * f);
}

static unsigned pvq_unquant(celt_bands_t *ctx, int32_t *x, int n, int k, int b_count, float gain) {
    int *iy = ctx->iy;
    pvq_decode(ctx->pvq, ctx->rc, iy, n, k);
    int64_t ryy = 0;
    for (int i = 0; i < n; i++) ryy += (int64_t)iy[i] * iy[i];
    const float f = gain * (float)NORM_ONE / sqrtf((float)ryy);
    for (int i = 0; i < n; i++) x[i] = (int32_t)lrintf((float)iy[i] * f);
    unspread(x, n, b_count, k, ctx->spread);
    if (b_count <= 1) return 1;
    const int n0 = n / b_count;
    unsigned mask = 0;
    for (int i = 0; i < b_count; i++) {
        for (int j = 0; j < n0; j++) {
            if (iy[i * n0 + j]) {
                mask |= 1u << i;
                break;
            }
        }
    }
    return mask;
}

static void haar1(int32_t *x, int n0, int stride) {
    const int32_t half_sqrt2 = 1518500250;
    n0 >>= 1;
    for (int i = 0; i < stride; i++) {
        for (int j = 0; j < n0; j++) {
            const int32_t a = opus_mulh(x[stride * 2 * j + i], half_sqrt2) * 2;
            const int32_t b = opus_mulh(x[stride * (2 * j + 1) + i], half_sqrt2) * 2;
            x[stride * 2 * j + i] = a + b;
            x[stride * (2 * j + 1) + i] = a - b;
        }
    }
}

static void deinterleave_hadamard(int32_t *x, int32_t *tmp, int n0, int stride, int hadamard) {
    const int n = n0 * stride;
    if (hadamard) {
        const uint8_t *order = celt_hadamard_order + stride - 2;
        for (int i = 0; i < stride; i++)
            for (int j = 0; j < n0; j++) tmp[order[i] * n0 + j] = x[j * stride + i];
    } else {
        for (int i = 0; i < stride; i++)
            for (int j = 0; j < n0; j++) tmp[i * n0 + j] = x[j * stride + i];
    }
    memcpy(x, tmp, (size_t)n * sizeof(*x));
}

static void interleave_hadamard(int32_t *x, int32_t *tmp, int n0, int stride, int hadamard) {
    const int n = n0 * stride;
    if (hadamard) {
        const uint8_t *order = celt_hadamard_order + stride - 2;
        for (int i = 0; i < stride; i++)
            for (int j = 0; j < n0; j++) tmp[j * stride + i] = x[order[i] * n0 + j];
    } else {
        for (int i = 0; i < stride; i++)
            for (int j = 0; j < n0; j++) tmp[j * stride + i] = x[i * n0 + j];
    }
    memcpy(x, tmp, (size_t)n * sizeof(*x));
}

static void stereo_merge(int32_t *x, int32_t *y, float mid, int n) {
    int64_t xp = 0;
    int64_t side = 0;
    for (int j = 0; j < n; j++) {
        xp += (int64_t)x[j] * y[j];
        side += (int64_t)y[j] * y[j];
    }
    const float scale = 1.0f / ((float)NORM_ONE * (float)NORM_ONE);
    const float fxp = mid * (float)xp * scale;
    const float fside = (float)side * scale;
    const float el = mid * mid + fside - 2.0f * fxp;
    const float er = mid * mid + fside + 2.0f * fxp;
    if (er < 6e-4f || el < 6e-4f) {
        memcpy(y, x, (size_t)n * sizeof(*y));
        return;
    }
    const float lgain = 1.0f / sqrtf(el);
    const float rgain = 1.0f / sqrtf(er);
    for (int j = 0; j < n; j++) {
        const float l = mid * (float)x[j];
        const float r = (float)y[j];
        x[j] = (int32_t)lrintf(lgain * (l - r));
        y[j] = (int32_t)lrintf(rgain * (l + r));
    }
}

static int compute_qn(int n, int b, int offset, int pulse_cap, int stereo) {
    int n2 = 2 * n - 1;
    if (stereo && n == 2) n2--;
    int qb = opus_imin(b - pulse_cap - (4 << BITRES), (b + n2 * offset) / n2);
    qb = opus_imin(8 << BITRES, qb);
    if (qb < (1 << BITRES >> 1)) return 1;
    const int qn = celt_exp2_frac8[qb & 7] >> (14 - (qb >> BITRES));
    return (qn + 1) >> 1 << 1;
}

static int decode_theta(celt_bands_t *ctx, int qn, int n, int b0, int stereo) {
    opus_rc_t *rc = ctx->rc;
    int itheta;
    if (stereo && n > 2) {
        const int p0 = 3;
        const int x0 = qn / 2;
        const int ft = p0 * (x0 + 1) + x0;
        const int fs = (int)opus_rc_decode(rc, (uint32_t)ft);
        const int x = fs < (x0 + 1) * p0 ? fs / p0 : x0 + 1 + (fs - (x0 + 1) * p0);
        const int fl = x <= x0 ? p0 * x : (x - 1 - x0) + (x0 + 1) * p0;
        const int fh = x <= x0 ? p0 * (x + 1) : (x - x0) + (x0 + 1) * p0;
        opus_rc_update(rc, (uint32_t)fl, (uint32_t)fh, (uint32_t)ft);
        itheta = x;
    } else if (b0 > 1 || stereo) {
        itheta = (int)opus_rc_uint(rc, (uint32_t)qn + 1);
    } else {
        const int half = qn >> 1;
        const int ft = (half + 1) * (half + 1);
        const int fm = (int)opus_rc_decode(rc, (uint32_t)ft);
        int fs, fl;
        if (fm < (half * (half + 1) >> 1)) {
            itheta = (int)(isqrt32(8 * (uint32_t)fm + 1) - 1) >> 1;
            fs = itheta + 1;
            fl = itheta * (itheta + 1) >> 1;
        } else {
            itheta = (2 * (qn + 1) - (int)isqrt32(8 * (uint32_t)(ft - fm - 1) + 1)) >> 1;
            fs = qn + 1 - itheta;
            fl = ft - ((qn + 1 - itheta) * (qn + 2 - itheta) >> 1);
        }
        opus_rc_update(rc, (uint32_t)fl, (uint32_t)(fl + fs), (uint32_t)ft);
    }
    return (int32_t)itheta * 16384 / qn;
}

static unsigned quant_band(celt_bands_t *ctx, int band, int32_t *x, int32_t *y, int n, int b,
                           int b_count, int tf_change, int32_t *lowband, int lm,
                           int32_t *lowband_out, int level, float gain, int32_t *lowband_scratch,
                           int fill) {
    const int n0 = n;
    int n_b = n;
    const int b0_in = b_count;
    int b0 = b_count;
    int time_divide = 0;
    int recombine = 0;
    int inv = 0;
    int imid = 0, iside = 0;
    float mid = 0.0f, side = 0.0f;
    unsigned cm = 0;
    const int long_blocks = b0_in == 1;
    const int stereo = y != NULL;
    int split = stereo;
    int32_t *tmp = ctx->scratch;

    n_b /= b_count;
    int n_b0 = n_b;

    if (n == 1) {
        int32_t *v = x;
        for (int c = 0; c < 1 + stereo; c++) {
            int sign = 0;
            if (ctx->remaining_bits >= 1 << BITRES) {
                sign = (int)opus_rc_bits(ctx->rc, 1);
                ctx->remaining_bits -= 1 << BITRES;
                b -= 1 << BITRES;
            }
            v[0] = sign ? -NORM_ONE : NORM_ONE;
            v = y;
        }
        if (lowband_out) lowband_out[0] = x[0];
        return 1;
    }

    if (!stereo && level == 0) {
        if (tf_change > 0) recombine = tf_change;
        if (lowband && (recombine || ((n_b & 1) == 0 && tf_change < 0) || b0 > 1)) {
            memcpy(lowband_scratch, lowband, (size_t)n * sizeof(*lowband));
            lowband = lowband_scratch;
        }
        for (int k = 0; k < recombine; k++) {
            static const uint8_t interleave[16] = { 0, 1, 1, 1, 2, 3, 3, 3, 2, 3, 3, 3, 2, 3, 3, 3 };
            if (lowband) haar1(lowband, n >> k, 1 << k);
            fill = interleave[fill & 0xF] | interleave[fill >> 4] << 2;
        }
        b_count >>= recombine;
        n_b <<= recombine;
        while ((n_b & 1) == 0 && tf_change < 0) {
            if (lowband) haar1(lowband, n_b, b_count);
            fill |= fill << b_count;
            b_count <<= 1;
            n_b >>= 1;
            time_divide++;
            tf_change++;
        }
        b0 = b_count;
        n_b0 = n_b;
        if (b0 > 1 && lowband) {
            deinterleave_hadamard(lowband, tmp, n_b >> recombine, b0 << recombine, long_blocks);
        }
    }

    const uint8_t *cache = pulse_cache(band, lm);
    if (!stereo && lm != -1 && b > cache[cache[0]] + 12 && n > 2) {
        n >>= 1;
        y = x + n;
        split = 1;
        lm -= 1;
        if (b_count == 1) fill = (fill & 1) | (fill << 1);
        b_count = (b_count + 1) >> 1;
    }

    if (split) {
        int itheta = 0;
        int delta;
        const int pulse_cap = celt_log_n[band] + lm * (1 << BITRES);
        const int offset = (pulse_cap >> 1) - (stereo && n == 2 ? QTHETA_OFFSET_TWOPHASE : QTHETA_OFFSET);
        int qn = compute_qn(n, b, offset, pulse_cap, stereo);
        if (stereo && band >= ctx->intensity) qn = 1;
        const int32_t tell = (int32_t)opus_rc_tell_frac(ctx->rc);
        if (qn != 1) {
            itheta = decode_theta(ctx, qn, n, b0, stereo);
        } else if (stereo) {
            if (b > 2 << BITRES && ctx->remaining_bits > 2 << BITRES) {
                inv = opus_rc_bit_logp(ctx->rc, 2);
            }
            itheta = 0;
        }
        const int qalloc = (int32_t)opus_rc_tell_frac(ctx->rc) - tell;
        b -= qalloc;
        const int orig_fill = fill;
        if (itheta == 0) {
            imid = 32767;
            iside = 0;
            fill &= (1 << b_count) - 1;
            delta = -16384;
        } else if (itheta == 16384) {
            imid = 0;
            iside = 32767;
            fill &= ((1 << b_count) - 1) << b_count;
            delta = 16384;
        } else {
            imid = bitexact_cos(itheta);
            iside = bitexact_cos(16384 - itheta);
            delta = frac_mul16((n - 1) << 7, bitexact_log2tan(iside, imid));
        }
        mid = (float)imid / 32768.0f;
        side = (float)iside / 32768.0f;

        if (n == 2 && stereo) {
            int sbits = 0;
            if (itheta != 0 && itheta != 16384) sbits = 1 << BITRES;
            const int mbits = b - sbits;
            const int c = itheta > 8192;
            ctx->remaining_bits -= qalloc + sbits;
            int32_t *x2 = c ? y : x;
            int32_t *y2 = c ? x : y;
            int sign = 0;
            if (sbits) sign = (int)opus_rc_bits(ctx->rc, 1);
            sign = 1 - 2 * sign;
            cm = quant_band(ctx, band, x2, NULL, n, mbits, b_count, tf_change, lowband, lm,
                            lowband_out, level, gain, lowband_scratch, orig_fill);
            y2[0] = -sign * x2[1];
            y2[1] = sign * x2[0];
            const int32_t qm = imid, qs = iside;
            const int32_t x0 = mul_q15(x[0], qm), x1 = mul_q15(x[1], qm);
            const int32_t y0 = mul_q15(y[0], qs), y1 = mul_q15(y[1], qs);
            x[0] = x0 - y0;
            y[0] = x0 + y0;
            x[1] = x1 - y1;
            y[1] = x1 + y1;
        } else {
            int32_t *next_lowband2 = NULL;
            int32_t *next_lowband_out1 = NULL;
            int next_level = 0;
            if (b0 > 1 && !stereo && (itheta & 0x3fff)) {
                if (itheta > 8192) {
                    delta -= delta >> (4 - lm);
                } else {
                    delta = opus_imin(0, delta + (n << BITRES >> (5 - lm)));
                }
            }
            int mbits = opus_imax(0, opus_imin(b, (b - delta) / 2));
            int sbits = b - mbits;
            ctx->remaining_bits -= qalloc;
            if (lowband && !stereo) next_lowband2 = lowband + n;
            if (stereo) {
                next_lowband_out1 = lowband_out;
            } else {
                next_level = level + 1;
            }
            int32_t rebalance = ctx->remaining_bits;
            const int side_shift = (b0 >> 1) & (stereo - 1);
            if (mbits >= sbits) {
                cm = quant_band(ctx, band, x, NULL, n, mbits, b_count, tf_change, lowband, lm,
                                next_lowband_out1, next_level, stereo ? 1.0f : gain * mid,
                                lowband_scratch, fill);
                rebalance = mbits - (rebalance - ctx->remaining_bits);
                if (rebalance > 3 << BITRES && itheta != 0) sbits += rebalance - (3 << BITRES);
                cm |= quant_band(ctx, band, y, NULL, n, sbits, b_count, tf_change, next_lowband2,
                                 lm, NULL, next_level, gain * side, NULL, fill >> b_count)
                      << side_shift;
            } else {
                cm = quant_band(ctx, band, y, NULL, n, sbits, b_count, tf_change, next_lowband2, lm,
                                NULL, next_level, gain * side, NULL, fill >> b_count)
                     << side_shift;
                rebalance = sbits - (rebalance - ctx->remaining_bits);
                if (rebalance > 3 << BITRES && itheta != 16384) mbits += rebalance - (3 << BITRES);
                cm |= quant_band(ctx, band, x, NULL, n, mbits, b_count, tf_change, lowband, lm,
                                 next_lowband_out1, next_level, stereo ? 1.0f : gain * mid,
                                 lowband_scratch, fill);
            }
        }
    } else {
        int q = celt_bits2pulses(band, lm, b);
        int curr_bits = celt_pulses2bits(band, lm, q);
        ctx->remaining_bits -= curr_bits;
        while (ctx->remaining_bits < 0 && q > 0) {
            ctx->remaining_bits += curr_bits;
            q--;
            curr_bits = celt_pulses2bits(band, lm, q);
            ctx->remaining_bits -= curr_bits;
        }
        if (q != 0) {
            cm = pvq_unquant(ctx, x, n, get_pulses(q), b_count, gain);
        } else {
            const unsigned cm_mask = (unsigned)(1ul << b_count) - 1;
            fill &= (int)cm_mask;
            if (!fill) {
                memset(x, 0, (size_t)n * sizeof(*x));
            } else {
                if (!lowband) {
                    for (int j = 0; j < n; j++) {
                        ctx->seed = celt_lcg(ctx->seed);
                        x[j] = (int32_t)ctx->seed >> 12;
                    }
                    cm = cm_mask;
                } else {
                    for (int j = 0; j < n; j++) {
                        ctx->seed = celt_lcg(ctx->seed);
                        const int32_t t = NORM_ONE / 256;
                        x[j] = lowband[j] + (ctx->seed & 0x8000 ? t : -t);
                    }
                    cm = (unsigned)fill;
                }
                celt_renormalise(x, n, gain);
            }
        }
    }

    if (stereo) {
        if (n != 2) stereo_merge(x, y, mid, n);
        if (inv && ctx->stereo_inversion) {
            for (int j = 0; j < n; j++) y[j] = -y[j];
        }
    } else if (level == 0) {
        if (b0 > 1) interleave_hadamard(x, tmp, n_b >> recombine, b0 << recombine, long_blocks);
        n_b = n_b0;
        b_count = b0;
        for (int k = 0; k < time_divide; k++) {
            b_count >>= 1;
            n_b <<= 1;
            cm |= cm >> b_count;
            haar1(x, n_b, b_count);
        }
        for (int k = 0; k < recombine; k++) {
            static const uint8_t deinterleave[16] = {
                0x00, 0x03, 0x0C, 0x0F, 0x30, 0x33, 0x3C, 0x3F,
                0xC0, 0xC3, 0xCC, 0xCF, 0xF0, 0xF3, 0xFC, 0xFF,
            };
            cm = deinterleave[cm];
            haar1(x, n0 >> k, 1 << k);
        }
        b_count <<= recombine;
        if (lowband_out) {
            const float scale = sqrtf((float)n0);
            for (int j = 0; j < n0; j++) lowband_out[j] = (int32_t)lrintf(scale * (float)x[j]);
        }
        cm &= (1u << b_count) - 1;
    }
    return cm;
}

void celt_quant_all_bands(celt_bands_t *ctx, int start, int end, int32_t *x_all, int32_t *y_all,
                          uint8_t *collapse, const int *pulses, int short_blocks,
                          int dual_stereo, const int *tf_res, int32_t total_bits,
                          int32_t balance, int32_t *norm) {
    const int lm = ctx->lm;
    const int m = 1 << lm;
    const int b_count = short_blocks ? m : 1;
    const int c_count = y_all ? 2 : 1;
    int32_t *norm2 = norm + m * celt_ebands[CELT_BANDS];
    int lowband_offset = 0;
    int update_lowband = 1;

    for (int i = start; i < end; i++) {
        int32_t *x = x_all + m * celt_ebands[i];
        int32_t *y = y_all ? y_all + m * celt_ebands[i] : NULL;
        const int n = m * celt_ebands[i + 1] - m * celt_ebands[i];
        const int32_t tell = (int32_t)opus_rc_tell_frac(ctx->rc);
        int b;
        int effective_lowband = -1;
        unsigned x_cm, y_cm;

        if (i != start) balance -= tell;
        ctx->remaining_bits = total_bits - tell - 1;
        if (i <= ctx->coded_bands - 1) {
            const int32_t curr_balance = balance / opus_imin(3, ctx->coded_bands - i);
            b = opus_imax(0, opus_imin(16383, opus_imin(ctx->remaining_bits + 1, pulses[i] + curr_balance)));
        } else {
            b = 0;
        }
        if ((m * celt_ebands[i] - n >= m * celt_ebands[start] || i == start + 1) &&
            (update_lowband || lowband_offset == 0)) {
            lowband_offset = i;
        }
        if (i == start + 1) {
            const int n1 = m * (celt_ebands[start + 1] - celt_ebands[start]);
            const int n2 = m * (celt_ebands[start + 2] - celt_ebands[start + 1]);
            const int offset = m * celt_ebands[start];
            if (n2 > n1) {
                memmove(&norm[offset + n1], &norm[offset + 2 * n1 - n2], (size_t)(n2 - n1) * sizeof(*norm));
                if (c_count == 2) {
                    memmove(&norm2[offset + n1], &norm2[offset + 2 * n1 - n2],
                            (size_t)(n2 - n1) * sizeof(*norm2));
                }
            }
        }
        const int tf_change = tf_res[i];
        if (lowband_offset != 0 && (ctx->spread != SPREAD_AGGRESSIVE || b_count > 1 || tf_change < 0)) {
            effective_lowband = opus_imax(m * celt_ebands[start], m * celt_ebands[lowband_offset] - n);
            int fold_start = lowband_offset;
            while (m * celt_ebands[--fold_start] > effective_lowband) {
            }
            int fold_end = lowband_offset - 1;
            while (++fold_end < i && m * celt_ebands[fold_end] < effective_lowband + n) {
            }
            x_cm = y_cm = 0;
            int fold_i = fold_start;
            do {
                x_cm |= collapse[fold_i * c_count];
                y_cm |= collapse[fold_i * c_count + c_count - 1];
            } while (++fold_i < fold_end);
        } else {
            x_cm = y_cm = (1u << b_count) - 1;
        }
        if (dual_stereo && i == ctx->intensity) {
            dual_stereo = 0;
            for (int j = m * celt_ebands[start]; j < m * celt_ebands[i]; j++) {
                norm[j] = (int32_t)(((int64_t)norm[j] + norm2[j]) >> 1);
            }
        }
        if (dual_stereo) {
            x_cm = quant_band(ctx, i, x, NULL, n, b / 2, b_count, tf_change,
                              effective_lowband != -1 ? norm + effective_lowband : NULL, lm,
                              norm + m * celt_ebands[i], 0, 1.0f, ctx->lowband_scratch, (int)x_cm);
            y_cm = quant_band(ctx, i, y, NULL, n, b / 2, b_count, tf_change,
                              effective_lowband != -1 ? norm2 + effective_lowband : NULL, lm,
                              norm2 + m * celt_ebands[i], 0, 1.0f, ctx->lowband_scratch, (int)y_cm);
        } else {
            x_cm = quant_band(ctx, i, x, y, n, b, b_count, tf_change,
                              effective_lowband != -1 ? norm + effective_lowband : NULL, lm,
                              norm + m * celt_ebands[i], 0, 1.0f, ctx->lowband_scratch,
                              (int)(x_cm | y_cm));
            y_cm = x_cm;
        }
        collapse[i * c_count] = (uint8_t)x_cm;
        collapse[i * c_count + c_count - 1] = (uint8_t)y_cm;
        balance += pulses[i] + tell;
        update_lowband = b > (n << BITRES);
    }
}

void celt_anti_collapse(int32_t *x_all, const uint8_t *collapse, int lm, int c_count, int size,
                        int start, int end, const int32_t *log_e, const int32_t *prev1,
                        const int32_t *prev2, const int *pulses, uint32_t seed) {
    for (int i = start; i < end; i++) {
        const int n0 = celt_ebands[i + 1] - celt_ebands[i];
        const int depth = (1 + pulses[i]) / (n0 << lm);
        const float thresh = 0.5f * opus_exp2(-0.125f * (float)depth);
        const float sqrt_1 = 1.0f / sqrtf((float)(n0 << lm));
        for (int c = 0; c < c_count; c++) {
            int32_t p1 = prev1[c * CELT_BANDS + i];
            int32_t p2 = prev2[c * CELT_BANDS + i];
            if (c_count == 1) {
                p1 = p1 > prev1[CELT_BANDS + i] ? p1 : prev1[CELT_BANDS + i];
                p2 = p2 > prev2[CELT_BANDS + i] ? p2 : prev2[CELT_BANDS + i];
            }
            int32_t ediff = log_e[c * CELT_BANDS + i] - (p1 < p2 ? p1 : p2);
            if (ediff < 0) ediff = 0;
            float r = 2.0f * opus_exp2(-(float)ediff / 1024.0f);
            if (lm == 3) r *= 1.41421356f;
            if (r > thresh) r = thresh;
            const int32_t v = (int32_t)lrintf(r * sqrt_1 * (float)NORM_ONE);
            int32_t *x = x_all + c * size + (celt_ebands[i] << lm);
            int renormalize = 0;
            for (int k = 0; k < 1 << lm; k++) {
                if (!(collapse[i * c_count + c] & 1u << k)) {
                    for (int j = 0; j < n0; j++) {
                        seed = celt_lcg(seed);
                        x[(j << lm) + k] = seed & 0x8000 ? v : -v;
                    }
                    renormalize = 1;
                }
            }
            if (renormalize) celt_renormalise(x, n0 << lm, 1.0f);
        }
    }
}
