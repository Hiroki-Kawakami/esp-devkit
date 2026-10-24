/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <string.h>

#include "audf_alloc.h"
#include "audf_mp3.h"
#include "audf_mp3_kernels.h"
#include "audf_mp3_tables.h"

static int32_t sat32(int64_t v) {
    return v > INT32_MAX ? INT32_MAX : v < INT32_MIN ? INT32_MIN : (int32_t)v;
}

void mp3_k_dot_c(int32_t *out, const int16_t *xh, const int16_t *xl, const int16_t *c, int rows, int n) {
    for (int r = 0; r < rows; r++, c += 2 * n) {
        const int16_t *ch = c, *cl = c + n;
        int64_t a = 0, b = 0;
        for (int k = 0; k < n; k++) {
            a += xh[k] * ch[k];
            b += xh[k] * cl[k] + xl[k] * ch[k];
        }
        out[r] = (int32_t)((uint32_t)sat32(a) + (uint32_t)sat32(b >> 16));
    }
}

void mp3_k_window_c(int32_t *acc, const int16_t *vh, const int16_t *vl) {
    for (int g = 0; g < 4; g++) {
        for (int l = 0; l < 8; l++) {
            const int j = 8 * g + l;
            int32_t sum = 0;
            for (int p = 0; p < 22; p++) {
                const int32_t c = mp3_window_split[(22 * g + p) * 8 + l];
                const int off = mp3_window_off[p] + j;
                sum += vh[off] * c + ((vl[off] * c) >> 16);
            }
            acc[j] = sum;
        }
    }
}

#if MP3_HAVE_PIE
static uint32_t next(uint32_t *s) {
    *s = *s * 1664525u + 1013904223u;
    return *s;
}
#endif

int audf_mp3_kernel_selftest(uint32_t seed, int iterations) {
#if MP3_HAVE_PIE
    struct {
        int16_t xh[24], xl[24];
        int16_t vh[1024], vl[1024];
        int32_t a[32], b[32];
    } *t = audf_malloc_aligned(16, sizeof(*t), 0);
    if (!t) return -1;
    int bad = 0;
    mp3_k_prepare();
    for (int it = 0; it < iterations; it++) {
        const int bits = 4 + (int)(next(&seed) % 25);
        for (int k = 0; k < 24; k++) mp3_split(t->xh + k, t->xl + k, (int32_t)next(&seed) >> (31 - bits));
        mp3_k_dot16(t->a, t->xh, t->xl, mp3_dct16e_split, 15);
        mp3_k_dot_c(t->b, t->xh, t->xl, mp3_dct16e_split, 15, 16);
        bad += memcmp(t->a, t->b, 15 * sizeof(int32_t)) != 0;
        mp3_k_dot16(t->a, t->xh, t->xl, mp3_dct16_split, 16);
        mp3_k_dot_c(t->b, t->xh, t->xl, mp3_dct16_split, 16, 16);
        bad += memcmp(t->a, t->b, 16 * sizeof(int32_t)) != 0;
        mp3_k_dot24(t->a, t->xh, t->xl, mp3_imdct18_split, 18);
        mp3_k_dot_c(t->b, t->xh, t->xl, mp3_imdct18_split, 18, 24);
        bad += memcmp(t->a, t->b, 18 * sizeof(int32_t)) != 0;
        for (int k = 0; k < 1024; k++) {
            mp3_split(t->vh + k, t->vl + k, (int32_t)next(&seed) >> (31 - (bits < 28 ? bits : 28)));
        }
        mp3_k_window(t->a, t->vh, t->vl, mp3_window_split);
        mp3_k_window_c(t->b, t->vh, t->vl);
        bad += memcmp(t->a, t->b, sizeof(t->a)) != 0;
    }
    audf_free(t);
    return bad;
#else
    (void)seed;
    (void)iterations;
    return 0;
#endif
}
