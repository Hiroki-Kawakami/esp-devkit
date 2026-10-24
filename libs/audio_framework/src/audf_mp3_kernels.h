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
#define MP3_HAVE_PIE 1
#else
#define MP3_HAVE_PIE 0
#endif

/* v as hi * 65536 + lo, lo signed. */
static inline void mp3_split(int16_t *hi, int16_t *lo, int32_t v) {
    *hi = (int16_t)((v + 0x8000) >> 16);
    *lo = (int16_t)(uint16_t)(uint32_t)v;
}

/* out[r] = sat(sum xh * ch) + sat((sum xh * cl + xl * ch) >> 16) over n inputs,
 * about sum (x * c) >> 32, for the [hi x n][lo x n] rows of c. n is 16 or 24;
 * x and c 16-byte aligned. */
void mp3_k_dot_c(int32_t *out, const int16_t *xh, const int16_t *xl, const int16_t *c, int rows, int n);

/* acc[j] for j < 32 = sum over the window rows p of vh[off_p + j] * c +
 * ((vl[off_p + j] * c) >> 16), with c = mp3_window_split for output j. */
void mp3_k_window_c(int32_t *acc, const int16_t *vh, const int16_t *vl);

#if MP3_HAVE_PIE
void mp3_k_prepare(void);
void mp3_k_dot16(int32_t *out, const int16_t *xh, const int16_t *xl, const int16_t *c, int rows);
void mp3_k_dot24(int32_t *out, const int16_t *xh, const int16_t *xl, const int16_t *c, int rows);
void mp3_k_window(int32_t *acc, const int16_t *vh, const int16_t *vl, const int16_t *c);
#else
static inline void mp3_k_prepare(void) {}
#define mp3_k_dot16(out, xh, xl, c, rows) mp3_k_dot_c(out, xh, xl, c, rows, 16)
#define mp3_k_dot24(out, xh, xl, c, rows) mp3_k_dot_c(out, xh, xl, c, rows, 24)
#define mp3_k_window(acc, vh, vl, c)      mp3_k_window_c(acc, vh, vl)
#endif
