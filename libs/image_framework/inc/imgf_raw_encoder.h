/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Raw (uncompressed, no container) packers covering display/storage formats
 * for embedded targets: L8 / I4 / I1 / RGB888 / RGB565. All share the same
 * factory because none of them have format-specific options.
 *
 * Each format has a fixed input pixfmt:
 *   L8 / I4 / I1  - IMGF_PIX_GRAY8 (I4/I1 expect indices in 0..N-1)
 *   RGB888        - IMGF_PIX_RGB888
 *   RGB565        - IMGF_PIX_RGB565 (host-endian uint16; bytes-side reorder
 *                                    is the LCD/DMA layer's job)
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
    IMGF_RAW_L8,        /* 8 bpp gray, passthrough */
    IMGF_RAW_I4,        /* 4 bpp indexed, first pixel MSB nibble */
    IMGF_RAW_I1,        /* 1 bpp indexed, first pixel MSB */
    IMGF_RAW_RGB888,    /* 24 bpp, passthrough */
    IMGF_RAW_RGB565,    /* 16 bpp, passthrough */
} imgf_raw_format_t;

typedef struct {
    uint32_t alloc_caps;
} imgf_raw_encoder_opts_t;

/* Format metadata reachable before any encoder is created (useful for the
 * caller to size the destination buffer up-front). */
size_t        imgf_raw_format_stride      (imgf_raw_format_t fmt, uint16_t width);
imgf_pixfmt_t imgf_raw_format_input_pixfmt(imgf_raw_format_t fmt);

imgf_encoder_t *imgf_raw_encoder_create(imgf_raw_format_t fmt,
                                        uint16_t width, uint16_t height,
                                        const imgf_raw_encoder_opts_t *opts,
                                        imgf_err_t *out_err);

#ifdef __cplusplus
}
#endif
