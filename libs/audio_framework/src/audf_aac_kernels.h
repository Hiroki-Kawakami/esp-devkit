/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdint.h>

#ifdef ESP_PLATFORM
#include "sdkconfig.h"
#endif

#if defined(ESP_PLATFORM) && defined(CONFIG_IDF_TARGET_ESP32P4)
#define AAC_HAVE_PIE 1
#else
#define AAC_HAVE_PIE 0
#endif

/* a * c / 2 for a Q31 c. */
static inline int32_t aac_mulh(int32_t a, int32_t c) {
    return (int32_t)(((int64_t)a * c) >> 32);
}

/* a * c / 2 for a Q15 c, floored per product so the PIE kernels can split a
 * into two 16-bit halves and still match. */
static inline int32_t aac_mulh16(int32_t a, int32_t c) {
    return (int32_t)(((int64_t)a * c) >> 16);
}

/* a * c for a Q15 c, floored per product, |a| below 2^30. */
static inline int32_t aac_mul15(int32_t a, int32_t c) {
    return aac_mulh(a * 2, c * 65536);
}

/* DCT-IV(128) in place scaled by 1/256; tmp holds 128 words. */
void aac_k_dct4_128(int32_t *x, int32_t *tmp);

/* The window kernels take each 32-bit sample as hi * 65536 + lo with lo signed
 * (aac_split), sum hi * c + ((lo * c) >> 16) and need 16-byte aligned operands. */
static inline void aac_split(int16_t *hi, int16_t *lo, int32_t v) {
    *hi = (int16_t)((v + 0x8000) >> 16);
    *lo = (int16_t)(uint16_t)(uint32_t)v;
}

/* u[p] = sum over j < 5 of x[p - 64 j] * c[64 j + p]. */
void aac_k_qmf_analysis_c(int32_t *u, const int16_t *hi, const int16_t *lo, const int16_t *c);

/* acc[k] = sum over t < 10 of v[(t / 2) * 256 + (t % 2) * 192 + k] * c[64 t + k]. */
void aac_k_qmf_synthesis_c(int32_t *acc, const int16_t *hi, const int16_t *lo, const int16_t *c);

/* DCT-IV(64) scaled by 1/128 of four interleaved transforms, x[k * 4 + lane],
 * with Q15 twiddles; tmp holds 256 words. */
void aac_k_dct4_64x4_c(int32_t *x, int32_t *tmp);

/* DCT-IV(1024) X of x scaled by 1/2048 with Q15 twiddles, left as x[k] = X[2k]
 * and x[512 + k] = X[1023 - 2k]; x and tmp 16-byte aligned, tmp holds 1024 words. */
void aac_k_dct4_1024_c(int32_t *x, int32_t *tmp);

/* a[k * 4 + l] = 16 * re(x[l][k]), b[(63 - k) * 4 + l] = 16 * im(x[l][k]). */
void aac_k_syn_in4_c(int32_t *a, int32_t *b, const int32_t *const x[4]);

/* The synthesis v of four slots from their DCT-IV halves, split into hi/lo;
 * slot l lands at hi - 128 * (l + 1) and lo - 128 * (l + 1). */
void aac_k_syn_v4_c(int16_t *hi, int16_t *lo, const int32_t *a, const int32_t *b);

/* PS kernels. A complex row of subbands is stored per group of four as
 * [re x4, im x4], k < 4 * groups, and each product is (x * c) >> 16.
 * all-pass: subbands 0-31 of the delayed input s through the three-link
 * all-pass ap ([3 links][8 slots][64]), writing slot w, into r.
 * gain: r *= 2 * g, g as [g x4, g x4] Q15.
 * mix: acc += step, then with c0-c3 the rounded upper halves of acc's four
 * [re x4, im x4] Q30 pairs per group, l, r = 4 * (l * c0 + r * c1),
 * 4 * (l * c2 + r * c3). mix_real: the same with only the re halves of the
 * pairs, which it alone steps. */
void aac_k_ps_allpass_c(int32_t *r, const int32_t *s, int32_t *ap, int w);
void aac_k_ps_gain_c(int32_t *r, const int16_t *g, int groups);
void aac_k_ps_mix_c(int32_t *l, int32_t *r, int32_t *acc, const int32_t *step, int groups);
void aac_k_ps_mix_real_c(int32_t *l, int32_t *r, int32_t *acc, const int32_t *step, int groups);
/* Between interleaved re/im pairs and the group layout. */
void aac_k_ps_unzip_c(int32_t *dst, const int32_t *src, int groups);
void aac_k_ps_zip_c(int32_t *dst, const int32_t *src, int groups);

/* (v + (1 << (shift - 1))) >> shift saturated to 16 bits, n samples a multiple
 * of 8, interleaved with b when b is not NULL. */
void aac_k_pack_c(int16_t *out, const int32_t *a, const int32_t *b, int n, int shift);

#if AAC_HAVE_PIE
void aac_k_prepare(void);
void aac_k_qmf_analysis(int32_t *u, const int16_t *hi, const int16_t *lo, const int16_t *c);
void aac_k_qmf_synthesis(int32_t *acc, const int16_t *hi, const int16_t *lo, const int16_t *c);
void aac_k_pack(int16_t *out, const int32_t *a, const int32_t *b, int n, int shift);
void aac_k_dct4_64x4(int32_t *x, int32_t *tmp);
void aac_k_dct4_1024(int32_t *x, int32_t *tmp);
void aac_k_ps_allpass(int32_t *r, const int32_t *s, int32_t *ap, int w);
void aac_k_ps_gain(int32_t *r, const int16_t *g, int groups);
void aac_k_ps_mix(int32_t *l, int32_t *r, int32_t *acc, const int32_t *step, int groups);
void aac_k_ps_mix_real(int32_t *l, int32_t *r, int32_t *acc, const int32_t *step, int groups);
void aac_k_ps_unzip(int32_t *dst, const int32_t *src, int groups);
void aac_k_ps_zip(int32_t *dst, const int32_t *src, int groups);
void aac_k_syn_in4(int32_t *a, int32_t *b, const int32_t *const x[4]);
void aac_k_syn_v4(int16_t *hi, int16_t *lo, const int32_t *a, const int32_t *b);
#else
#define aac_k_syn_in4 aac_k_syn_in4_c
#define aac_k_syn_v4 aac_k_syn_v4_c
#define aac_k_dct4_64x4 aac_k_dct4_64x4_c
#define aac_k_dct4_1024 aac_k_dct4_1024_c
#define aac_k_ps_allpass aac_k_ps_allpass_c
#define aac_k_ps_gain aac_k_ps_gain_c
#define aac_k_ps_mix aac_k_ps_mix_c
#define aac_k_ps_mix_real aac_k_ps_mix_real_c
#define aac_k_ps_unzip aac_k_ps_unzip_c
#define aac_k_ps_zip aac_k_ps_zip_c
static inline void aac_k_prepare(void) {}
#define aac_k_qmf_analysis aac_k_qmf_analysis_c
#define aac_k_qmf_synthesis aac_k_qmf_synthesis_c
#define aac_k_pack aac_k_pack_c
#endif
