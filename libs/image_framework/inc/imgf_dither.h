/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "imgf_types.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    IMGF_DITHER_NONE = 0,            /* nearest-level quantize, no dither */
    IMGF_DITHER_BAYER2,              /* ordered, stateless, frame-stable */
    IMGF_DITHER_BAYER4,
    IMGF_DITHER_BAYER8,
    IMGF_DITHER_FLOYD_STEINBERG,     /* error diffusion */
    IMGF_DITHER_ATKINSON,
    IMGF_DITHER_SIERRA,
    IMGF_DITHER_JJN,                 /* Jarvis-Judice-Ninke */
    IMGF_DITHER_STUCKI,
} imgf_dither_algo_t;

typedef enum {
    IMGF_DITHER_OUT_INDEX = 0,       /* uint8_t indices 0..N-1 (for downstream packers) */
    IMGF_DITHER_OUT_GRAY,            /* reconstructed Gray8 = level * 255 / (N-1) */
} imgf_dither_out_t;

typedef struct {
    imgf_dither_algo_t algo;         /* default IMGF_DITHER_NONE */
    uint8_t            levels;       /* N: 2..256 (0 -> 16) */
    bool               serpentine;   /* boustrophedon scan; error-diffusion only */
    imgf_dither_out_t  out_mode;     /* default INDEX */
    uint32_t           alloc_caps;
} imgf_dither_opts_t;

typedef struct imgf_dither imgf_dither_t;

imgf_dither_t  *imgf_dither_create(uint16_t width,
                                   const imgf_dither_opts_t *opts,
                                   imgf_err_t *out_err);
void            imgf_dither_destroy   (imgf_dither_t *d);

uint8_t           imgf_dither_levels   (const imgf_dither_t *d);
imgf_dither_out_t imgf_dither_out_mode (const imgf_dither_t *d);

/* Streaming model: caller pushes Gray8 rows in scan order; each push yields
 * exactly one ready output row. Output is uint8_t per pixel (semantics from
 * out_mode). Returns 1 on success, -1 on error (see imgf_dither_last_error). */
int             imgf_dither_push_row(imgf_dither_t *d, const uint8_t *gray_row);
bool            imgf_dither_pop_row (imgf_dither_t *d, uint8_t *out_row);

imgf_err_t      imgf_dither_last_error(const imgf_dither_t *d);

/* Layer 2: process a whole Gray8 buffer in one call. dst may overlap src
 * (e.g. dst == src for in-place). Stride 0 = tight packing (= width). */
imgf_err_t imgf_dither_buffer(const uint8_t *src, uint16_t w, uint16_t h,
                              size_t src_stride,
                              uint8_t *dst, size_t dst_stride,
                              const imgf_dither_opts_t *opts);

#ifdef __cplusplus
}
#endif
