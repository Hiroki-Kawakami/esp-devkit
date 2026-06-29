/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Output sink at the end of an image_framework pipeline. The format enum spans
 * trivial raw packers (L8 / I4 / I1 / RGB888 / RGB565) and, when added, full
 * container encoders (PNG / JPEG / ...). The streaming surface (create + bind
 * + push_row + finish) is the same; only the per-format internals differ.
 *
 * Each format has a fixed input pixfmt; chain upstream (resize / dither) is
 * responsible for delivering that shape. Use imgf_encoder_input_pixfmt() to
 * confirm what to feed.
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
    /* PNG / JPEG come later — same API, just new enum values. */
} imgf_enc_format_t;

typedef struct {
    uint32_t alloc_caps;
} imgf_encoder_opts_t;

/* Bytes per packed row of a given format at `width` pixels. Useful when the
 * caller sizes a destination buffer before creating the encoder. */
size_t imgf_encoder_format_stride(imgf_enc_format_t fmt, uint16_t width);

/* Expected input pixel format for the given output format. Returned by
 * imgf_encoder_input_pixfmt() but also as a free function for pre-allocation
 * choices. */
imgf_pixfmt_t imgf_encoder_format_input_pixfmt(imgf_enc_format_t fmt);

typedef struct imgf_encoder imgf_encoder_t;

imgf_encoder_t *imgf_encoder_create(imgf_enc_format_t fmt,
                                    uint16_t width, uint16_t height,
                                    const imgf_encoder_opts_t *opts,
                                    imgf_err_t *out_err);
void            imgf_encoder_destroy(imgf_encoder_t *e);

size_t          imgf_encoder_row_stride   (const imgf_encoder_t *e);
size_t          imgf_encoder_buffer_size  (const imgf_encoder_t *e);
imgf_pixfmt_t   imgf_encoder_input_pixfmt (const imgf_encoder_t *e);

/* Hand the encoder a writable buffer of >= imgf_encoder_buffer_size() bytes.
 * For raw formats the encoder writes rows directly; container formats (when
 * added) may need their own scratch and grow into this buffer incrementally. */
imgf_err_t      imgf_encoder_bind_buffer(imgf_encoder_t *e, uint8_t *dst, size_t cap);

/* Feed one row in scan order. Returns 1 on success, -1 on error. */
int             imgf_encoder_push_row(imgf_encoder_t *e, const uint8_t *row);

/* Finalize the output (no-op for raw formats; writes the container trailer for
 * PNG/JPEG/... when added). On return, *bytes_written reports the total bytes
 * the encoder consumed in the destination buffer. */
imgf_err_t      imgf_encoder_finish(imgf_encoder_t *e, size_t *bytes_written);

imgf_err_t      imgf_encoder_last_error(const imgf_encoder_t *e);

/* Layer 2: encode an entire src image into dst in one call. dst capacity must
 * be >= imgf_encoder_format_stride(fmt, w) * h (for raw formats). Returns the
 * actual bytes written via *out_bytes (NULL = ignored). */
imgf_err_t imgf_encode_buffer(imgf_enc_format_t fmt,
                              const uint8_t *src, uint16_t w, uint16_t h,
                              size_t src_stride,
                              uint8_t *dst, size_t dst_capacity,
                              size_t *out_bytes,
                              const imgf_encoder_opts_t *opts);

#ifdef __cplusplus
}
#endif
