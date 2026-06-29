/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Common types shared across image_framework modules (decoders, resizer,
 * future encoders / dither / color). Putting them in one header keeps new
 * modules from having to chase per-concept includes.
 */

#pragma once

#ifdef __cplusplus
extern "C" {
#endif

/* ---- error code ------------------------------------------------------- */

typedef int imgf_err_t;

#define IMGF_OK                  0
#define IMGF_ERR_TRUNCATED       1
#define IMGF_ERR_DECODE          2
#define IMGF_ERR_UNSUPPORTED     3
#define IMGF_ERR_OOM             4
#define IMGF_ERR_INVALID_STATE   5
#define IMGF_ERR_INVALID_ARG     6
#define IMGF_ERR_TOO_LARGE       7

const char *imgf_err_to_str(imgf_err_t err);

/* ---- pixel format ----------------------------------------------------- */

typedef enum {
    IMGF_PIX_INHERIT = 0,   /* sentinel for opts: "same as the source pixfmt" */
    IMGF_PIX_GRAY8,
    IMGF_PIX_RGB888,
    IMGF_PIX_RGB565,        /* host-endian uint16; LCD/DMA-side byte order is caller's job */
} imgf_pixfmt_t;

static inline int imgf_pixfmt_bpp(imgf_pixfmt_t pf) {
    switch (pf) {
        case IMGF_PIX_GRAY8:  return 1;
        case IMGF_PIX_RGB888: return 3;
        case IMGF_PIX_RGB565: return 2;
        default:              return 0;
    }
}

#ifdef __cplusplus
}
#endif
