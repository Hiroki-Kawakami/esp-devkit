/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ESP32-P4 PIE kernels (src/imgf_pie_p4.S). Each one has a C counterpart that
 * writes the same bytes; IMGF_HAVE_PIE picks between them.
 */

#pragma once

#include <stdint.h>

#if defined(ESP_PLATFORM) && defined(CONFIG_IDF_TARGET_ESP32P4)
#define IMGF_HAVE_PIE 1
#else
#define IMGF_HAVE_PIE 0
#endif

#if IMGF_HAVE_PIE

#ifdef __cplusplus
extern "C" {
#endif

void imgf_k_prepare(void);

/* n16 groups of 16 pixels, YCbCr planes to interleaved RGB888. _h1 takes
 * chroma at half the luma width. Both read 16 chroma bytes per group, so the
 * source planes need 16 bytes of slack past their last row. */
void imgf_k_ycc_rgb_h1(uint8_t *dst, const uint8_t *y,
                       const uint8_t *cb, const uint8_t *cr, int n16);
void imgf_k_ycc_rgb_h0(uint8_t *dst, const uint8_t *y,
                       const uint8_t *cb, const uint8_t *cr, int n16);

/* IDCT of dequantized coefficients in natural order, level-shifted and clamped
 * to bytes. rowmask bit v marks row v as having a non-zero coefficient. `tab`
 * picks the output size; the sub-sampled tables still write 8x8, with the
 * answer in the top-left, so their callers pass a scratch block. */
void imgf_k_idct(uint8_t *dst, int stride, const int16_t *dqc, int rowmask,
                 const int16_t *tab);

extern const int16_t imgf_idct_tab8[], imgf_idct_tab4[], imgf_idct_tab2[];

/* Resizer row packing. Inputs are 0..255 in 16-bit lanes; n16 counts groups of
 * 16 samples, n8 groups of 8 R,G,B triplets. */
void imgf_k_pack_bytes (uint8_t *dst, const uint16_t *src, int n16);
void imgf_k_pack_rgb565(uint8_t *dst, const uint16_t *src, int n8);

#ifdef __cplusplus
}
#endif

#endif
