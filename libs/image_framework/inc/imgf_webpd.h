/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include "imgf_decoder.h"

#ifdef __cplusplus
extern "C" {
#endif

/* WebP decoder: lossy, lossless and lossy with an alpha plane; of an animation
 * only the first frame, on its canvas. Always RGB888, with alpha composited
 * over white and the canvas outside the frame white. Lossy and alpha need a
 * stream with seek() and return IMGF_ERR_UNSUPPORTED from open() without one.
 * Lossless holds up to about 1M pixels (4 bytes each) of the picture out of
 * `alloc_caps`, the distance a back-reference can reach. */
imgf_decoder_t *imgf_webpd_create(void);

#ifdef __cplusplus
}
#endif
