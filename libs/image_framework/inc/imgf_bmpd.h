/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include "imgf_decoder.h"

#ifdef __cplusplus
extern "C" {
#endif

/* BMP decoder: 1/4/8-bit palette, 16/24/32-bit (BI_RGB and BI_BITFIELDS),
 * RLE4 and RLE8, top-down and bottom-up, OS/2 1.x and Windows 3.x-5 headers.
 * Always RGB888; alpha is ignored. A bottom-up picture needs a stream with
 * seek(). An RLE picture is expanded whole on the first next_row() into one
 * byte per pixel out of `alloc_caps`; pixels a delta skips are palette
 * entry 0. */
imgf_decoder_t *imgf_bmpd_create(void);

#ifdef __cplusplus
}
#endif
