/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include "imgf_decoder.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Self-contained JPEG decoder (SOF0 baseline and SOF2 progressive, 8-bit,
 * Huffman). Supports grayscale + YCbCr with sampling factors <= 2x2 (4:4:4 /
 * 4:2:2 / 4:4:0 / 4:2:0) and restart intervals. Downscales while decoding by a
 * 1/1..1/8 factor chosen from imgf_decode_opts_t. 16-bit / arithmetic-coded /
 * CMYK streams return IMGF_ERR_UNSUPPORTED from open().
 *
 * A progressive stream is read whole on the first next_row() call, which costs
 * one int16 coefficient buffer per component (w*h*2 bytes at 4:4:4, ~3 MB for
 * a 1000x1000 4:2:0 picture) out of `alloc_caps`; the downscale factor does
 * not shrink it. A truncated or damaged stream keeps the scans that arrived
 * instead of failing. Baseline decoding still streams within a band. */
imgf_decoder_t *imgf_jpegd_create(void);

#ifdef __cplusplus
}
#endif
