/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Baseline (SOF0) JPEG encoder. 4:4:4 / 4:2:2 / 4:2:0 chroma subsampling for
 * color; grayscale is always single-component. Standard Annex K Huffman and
 * quantization tables (luma + chroma). Quality 1-100. Hot path is integer-
 * only (integer AAN FDCT + integer quantization).
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "imgf_encoder.h"
#include "imgf_types.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    IMGF_JPEG_SUBSAMPLE_DEFAULT = 0,  /* color -> 4:2:0; ignored for gray */
    IMGF_JPEG_SUBSAMPLE_444,
    IMGF_JPEG_SUBSAMPLE_422,
    IMGF_JPEG_SUBSAMPLE_420,
} imgf_jpeg_subsample_t;

typedef struct {
    int                   quality;      /* 1..100; 0 -> default 75 */
    imgf_jpeg_subsample_t subsample;
    uint32_t              alloc_caps;
} imgf_jpege_opts_t;

/* Worst-case upper bound on the encoded byte count for sizing the dst buffer
 * before create (header + ~2 bytes/pixel for very low quality + slack). */
size_t imgf_jpege_buffer_upper_bound(uint16_t w, uint16_t h, imgf_pixfmt_t input_pf);

/* input_pf must be IMGF_PIX_GRAY8 or IMGF_PIX_RGB888 (RGB565 unsupported;
 * convert with the resizer first). */
imgf_encoder_t *imgf_jpege_create(uint16_t width, uint16_t height,
                                  imgf_pixfmt_t input_pf,
                                  const imgf_jpege_opts_t *opts,
                                  imgf_err_t *out_err);

#ifdef __cplusplus
}
#endif
