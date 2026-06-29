/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Common encoder abstraction. Concrete encoders are constructed by per-format
 * factories (imgf_jpege_create, imgf_raw_encoder_create, ...); each returns
 * an imgf_encoder_t * the rest of the pipeline can drive through the uniform
 * push_row / finish surface defined here. Mirrors the imgf_decoder_t shape.
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "imgf_types.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct imgf_encoder imgf_encoder_t;

/* Hand the encoder a writable destination buffer. Capacity must be >=
 * imgf_encoder_buffer_size(e). For container encoders the header is written
 * at bind time; the trailer is written by finish(). */
imgf_err_t      imgf_encoder_bind_buffer (imgf_encoder_t *e, uint8_t *dst, size_t cap);

/* Feed one row of source pixels (format determined by the concrete encoder's
 * expected input pixfmt — query via imgf_encoder_input_pixfmt). Returns 1 on
 * success, -1 on error (see imgf_encoder_last_error). */
int             imgf_encoder_push_row    (imgf_encoder_t *e, const uint8_t *row);

/* Finalize the output. On return, *bytes_written reports the total bytes
 * consumed in the destination buffer (NULL = ignored). */
imgf_err_t      imgf_encoder_finish      (imgf_encoder_t *e, size_t *bytes_written);

/* Row stride of the bound destination (raw formats only; 0 for variable-rate
 * container formats like JPEG). */
size_t          imgf_encoder_row_stride  (const imgf_encoder_t *e);

/* Pre-allocation guidance for the destination buffer: exact size for raw
 * formats, a generous worst-case upper bound for container formats. */
size_t          imgf_encoder_buffer_size (const imgf_encoder_t *e);

/* Pixel format the encoder expects for push_row input. */
imgf_pixfmt_t   imgf_encoder_input_pixfmt(const imgf_encoder_t *e);

imgf_err_t      imgf_encoder_last_error  (const imgf_encoder_t *e);

void            imgf_encoder_destroy     (imgf_encoder_t *e);

/* Layer 2: bind, push every row, finish — for an already-created encoder.
 * `src_stride` is the source row stride in bytes (0 -> tight = w * bpp of the
 * encoder's input pixfmt). Returns total bytes written via *out_bytes. */
imgf_err_t      imgf_encoder_encode_buffer(imgf_encoder_t *e,
                                           const uint8_t *src, size_t src_stride,
                                           uint8_t *dst, size_t dst_cap,
                                           size_t *out_bytes);

#ifdef __cplusplus
}
#endif
