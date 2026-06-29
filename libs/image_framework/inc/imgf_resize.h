/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "imgf_decoder.h"
#include "imgf_types.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    IMGF_FIT_STRETCH,   /* exact target_w x target_h */
    IMGF_FIT_CONTAIN,   /* aspect-preserved, fit inside target_w x target_h
                           (target_w=0 or target_h=0 -> that axis is unbounded;
                            both zero -> identity sizing) */
} imgf_fit_t;

typedef struct {
    uint16_t      target_w;
    uint16_t      target_h;
    imgf_fit_t    fit;
    imgf_pixfmt_t dst_pixfmt;     /* IMGF_PIX_INHERIT (0) = src と同じ */
    uint32_t      alloc_caps;
} imgf_resize_opts_t;

/* Resolves destination geometry without allocating. Useful when the caller
 * needs to know dst dims before allocating a destination buffer (e.g. for
 * imgf_resize_buffer with Contain). */
imgf_err_t imgf_resize_compute_dst(uint16_t src_w, uint16_t src_h,
                                   const imgf_resize_opts_t *opts,
                                   uint16_t *out_w, uint16_t *out_h);


/* ---- Layer 1: streaming resizer ----
 * Push source rows in scan order, pop destination rows as they become ready.
 * Per-axis algorithm: box-area average for downscale, bilinear for upscale,
 * identity at unchanged dimension. Caller must drain all rows the previous
 * push reported as ready before pushing the next row. After the last src
 * row, call imgf_resizer_finish() to flush any residual partial dst row. */
typedef struct imgf_resizer imgf_resizer_t;

imgf_resizer_t *imgf_resizer_create(uint16_t src_w, uint16_t src_h,
                                    imgf_pixfmt_t src_pf,
                                    const imgf_resize_opts_t *opts,
                                    imgf_err_t *out_err);
void            imgf_resizer_destroy   (imgf_resizer_t *r);

uint16_t        imgf_resizer_dst_width (const imgf_resizer_t *r);
uint16_t        imgf_resizer_dst_height(const imgf_resizer_t *r);
imgf_pixfmt_t   imgf_resizer_dst_pixfmt(const imgf_resizer_t *r);

/* Returns the number of destination rows that became newly ready as a result
 * of consuming this src row (0 for downscale-v that hasn't completed a dst
 * row yet, possibly > 1 for upscale-v). Returns -1 on error
 * (imgf_resizer_last_error reveals which). */
int             imgf_resizer_push_row  (imgf_resizer_t *r, const uint8_t *src_row);

/* Drains one ready destination row. Returns true if a row was written into
 * dst_row, false if no row is currently ready or all rows already popped. */
bool            imgf_resizer_pop_row   (imgf_resizer_t *r, uint8_t *dst_row);

/* After pushing all source rows, ask the resizer to finalize any residual
 * partial destination row (downscale-v with rounding leftover). Returns 1 if
 * a new row became ready, 0 otherwise. After finish() no further push is
 * permitted. */
int             imgf_resizer_finish    (imgf_resizer_t *r);

imgf_err_t      imgf_resizer_last_error(const imgf_resizer_t *r);


/* ---- Layer 2: convenience wrappers ----
 * Common shapes that compose the streaming core with a row source/sink. */

/* Pulls rows from an opened decoder and returns a heap-allocated buffer of
 * size out_w * out_h * imgf_pixfmt_bpp(dst_pf). Caller frees with imgf_free.
 * The decoder's open() should already be set with hints (e.g. JPEG's coarse
 * 1/N downscale) matching opts.target_w/h for best efficiency. */
imgf_err_t imgf_resize_decoder(imgf_decoder_t *dec,
                               const imgf_resize_opts_t *opts,
                               uint8_t **out_data,
                               uint16_t *out_w, uint16_t *out_h);

/* Resizes the fully-decoded buffer at `src` (src_w x src_h, src_pf) into
 * `dst`. dst_stride is the destination row stride in bytes; pass 0 for
 * tight packing (= dst_w * imgf_pixfmt_bpp(dst_pf)). dst may overlap src
 * (e.g. dst == src) ONLY when the resize is a downscale on every axis;
 * upscale + overlap returns IMGF_ERR_INVALID_ARG. */
imgf_err_t imgf_resize_buffer(const uint8_t *src, uint16_t src_w, uint16_t src_h,
                              size_t src_stride, imgf_pixfmt_t src_pf,
                              uint8_t *dst, size_t dst_stride,
                              const imgf_resize_opts_t *opts);

#ifdef __cplusplus
}
#endif
