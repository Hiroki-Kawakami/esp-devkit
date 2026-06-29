/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include "imgf_decoder.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Self-contained baseline JPEG decoder (SOF0, 8-bit, Huffman). Supports
 * grayscale + YCbCr with sampling factors <= 2x2 (4:4:4 / 4:2:2 / 4:4:0 /
 * 4:2:0) and restart intervals. Downscales while decoding by a 1/1..1/8 factor
 * chosen from imgf_decode_opts_t. Progressive / 16-bit / arithmetic-coded /
 * CMYK streams return IMGF_ERR_UNSUPPORTED from open(). */
imgf_decoder_t *imgf_jpegd_create(void);

#ifdef __cplusplus
}
#endif
