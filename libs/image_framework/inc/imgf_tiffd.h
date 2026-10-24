/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include "imgf_decoder.h"

#ifdef __cplusplus
extern "C" {
#endif

/* TIFF decoder for the first IFD: strips or tiles, uncompressed, PackBits, LZW
 * or Deflate, horizontal predictor; bilevel/grayscale (1-16 bit), palette
 * (1-8 bit) and RGB (8/16 bit), with an alpha extra sample composited over
 * white. 16-bit samples keep their high byte. JPEG/CCITT compression, CMYK,
 * YCbCr, separate planes, floating point and BigTIFF return
 * IMGF_ERR_UNSUPPORTED from open(), as does a stream without seek(). A tiled
 * picture holds one row of tiles, decoded, out of `alloc_caps`. */
imgf_decoder_t *imgf_tiffd_create(void);

#ifdef __cplusplus
}
#endif
