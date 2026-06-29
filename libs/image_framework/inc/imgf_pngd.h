/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include "imgf_decoder.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Self-contained PNG decoder: streaming inflate -> scanline unfilter -> per-row
 * color normalization. Supports 8-bit grayscale/RGB/RGBA, 1/2/4/8-bit
 * grayscale and palette, with tRNS composited over white. Returns
 * IMGF_ERR_UNSUPPORTED from open() for 16-bit channels and interlaced
 * streams. */
imgf_decoder_t *imgf_pngd_create(void);

#ifdef __cplusplus
}
#endif
