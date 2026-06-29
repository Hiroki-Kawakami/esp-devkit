/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Output sink at the end of an image_framework pipeline. The format enum spans
 * trivial raw packers (L8 / I4 / I1 / RGB888 / RGB565) and container encoders
 * (JPEG; PNG to follow). The streaming surface (create + bind + push_row +
 * finish) is the same; only the per-format internals differ.
 *
 * For raw formats the expected input pixfmt is fixed (Gray8 for L8/I4/I1, the
 * matching pf for RGB888/RGB565). For JPEG it can be Gray8 or RGB888 (RGB565
 * unsupported; convert with the resizer first). The input pixfmt is passed
 * explicitly at create; pass IMGF_PIX_INHERIT to take the format's default.
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "imgf_types.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    IMGF_ENC_L8,        /* input Gray8: passthrough. */
    IMGF_ENC_I4,        /* input Gray8-shaped indices 0..15: 4 bpp, first pixel MSB nibble. */
    IMGF_ENC_I1,        /* input Gray8-shaped indices 0..1:  1 bpp, first pixel MSB. */
    IMGF_ENC_RGB888,    /* input RGB888: passthrough. */
    IMGF_ENC_RGB565,    /* input RGB565 host-endian uint16: passthrough. */
    IMGF_ENC_JPEG,      /* input Gray8 or RGB888; baseline 4:4:4 JPEG output. */
    /* PNG comes later — same API, just a new enum value. */
} imgf_enc_format_t;

typedef enum {
    IMGF_JPEG_SUBSAMPLE_DEFAULT = 0,  /* implementation default: 4:2:0 for color, n/a for gray */
    IMGF_JPEG_SUBSAMPLE_444,          /* no chroma subsampling */
    IMGF_JPEG_SUBSAMPLE_422,          /* 2:1 horizontal */
    IMGF_JPEG_SUBSAMPLE_420,          /* 2:1 both axes */
} imgf_jpeg_subsample_t;

typedef struct {
    uint32_t alloc_caps;
    /* JPEG-only; ignored by other formats. */
    int                   jpeg_quality;     /* 1..100; 0 -> default 75. */
    imgf_jpeg_subsample_t jpeg_subsample;   /* 0 -> 4:2:0 (color); ignored for gray. */
} imgf_encoder_opts_t;

/* Bytes per packed row of a raw format at `width` pixels. Returns 0 for
 * variable-rate formats (JPEG); for those the caller sizes the destination
 * buffer at create time via imgf_encoder_buffer_size(). */
size_t        imgf_encoder_format_stride(imgf_enc_format_t fmt, uint16_t width);

/* Required input pixfmt for the format, or IMGF_PIX_INHERIT if the format
 * accepts multiple input pixfmts (currently JPEG: Gray8 or RGB888). */
imgf_pixfmt_t imgf_encoder_format_input_pixfmt(imgf_enc_format_t fmt);

typedef struct imgf_encoder imgf_encoder_t;

imgf_encoder_t *imgf_encoder_create(imgf_enc_format_t fmt,
                                    uint16_t width, uint16_t height,
                                    imgf_pixfmt_t input_pixfmt,
                                    const imgf_encoder_opts_t *opts,
                                    imgf_err_t *out_err);
void            imgf_encoder_destroy(imgf_encoder_t *e);

/* Row stride of the bound destination buffer (raw formats only; 0 for JPEG). */
size_t          imgf_encoder_row_stride   (const imgf_encoder_t *e);
/* Pre-allocation guidance for the destination buffer: exact size for raw
 * formats, a generous worst-case upper bound for JPEG (caller may pass less if
 * they know their content compresses well, but bind_buffer will fail if
 * encoding overruns). */
size_t          imgf_encoder_buffer_size  (const imgf_encoder_t *e);
imgf_pixfmt_t   imgf_encoder_input_pixfmt (const imgf_encoder_t *e);

/* Hand the encoder a writable buffer. For raw formats the encoder writes
 * directly at row offsets; container encoders write a header at bind, then
 * append per push_row, then a trailer at finish. */
imgf_err_t      imgf_encoder_bind_buffer(imgf_encoder_t *e, uint8_t *dst, size_t cap);

/* Feed one row in scan order. Returns 1 on success, -1 on error. */
int             imgf_encoder_push_row(imgf_encoder_t *e, const uint8_t *row);

/* Finalize the output. On return, *bytes_written reports total bytes consumed
 * in the destination buffer. */
imgf_err_t      imgf_encoder_finish(imgf_encoder_t *e, size_t *bytes_written);

imgf_err_t      imgf_encoder_last_error(const imgf_encoder_t *e);

/* Layer 2: encode an entire src image into dst in one call. Returns the actual
 * bytes written via *out_bytes (NULL = ignored). For JPEG, dst_capacity must
 * be generous (a safe upper bound is width * height * channels + 1024). */
imgf_err_t imgf_encode_buffer(imgf_enc_format_t fmt,
                              const uint8_t *src, uint16_t w, uint16_t h,
                              size_t src_stride, imgf_pixfmt_t src_pf,
                              uint8_t *dst, size_t dst_capacity,
                              size_t *out_bytes,
                              const imgf_encoder_opts_t *opts);

#ifdef __cplusplus
}
#endif
