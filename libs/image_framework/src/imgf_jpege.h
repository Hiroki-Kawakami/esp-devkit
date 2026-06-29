/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Internal: baseline (SOF0) JPEG encoder used by imgf_encoder when
 * IMGF_ENC_JPEG is selected. 4:4:4 sampling only; Gray8 or RGB888 input.
 * Standard Annex K Huffman + quantization tables (luma + chroma), quality 1-100.
 *
 * Hosted under imgf_encoder.c — these functions are not part of the public API.
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "imgf_types.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct imgf_jpege imgf_jpege_t;

imgf_jpege_t *imgf_jpege_create(uint16_t width, uint16_t height,
                                imgf_pixfmt_t input_pf,
                                int quality, int subsample,
                                uint32_t alloc_caps, imgf_err_t *out_err);
void          imgf_jpege_destroy(imgf_jpege_t *j);

size_t        imgf_jpege_buffer_upper_bound(uint16_t w, uint16_t h, imgf_pixfmt_t pf);

imgf_err_t    imgf_jpege_bind  (imgf_jpege_t *j, uint8_t *dst, size_t cap);
imgf_err_t    imgf_jpege_push  (imgf_jpege_t *j, const uint8_t *row);
imgf_err_t    imgf_jpege_finish(imgf_jpege_t *j, size_t *bytes_written);

#ifdef __cplusplus
}
#endif
