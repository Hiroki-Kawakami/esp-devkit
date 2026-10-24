/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Shared between the WebP container (imgf_webpd.c) and its two codecs.
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "imgf_stream.h"
#include "imgf_types.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    imgf_stream_t stream;
    size_t        at;
    bool          truncated;
} webp_source_t;

/* Reads `n` bytes at `pos`. Without seek() only forward moves work. */
size_t webp_source_read(webp_source_t *src, size_t pos, void *dst, size_t n);

typedef struct {
    webp_source_t *src;
    uint8_t       *buf;
    size_t         cap;
    size_t         next;
    size_t         end;
} webp_cursor_t;

bool webp_cursor_init(webp_cursor_t *c, webp_source_t *src, size_t start, size_t len,
                      size_t cap, uint32_t caps);
void webp_cursor_deinit(webp_cursor_t *c);
/* The next block of the range; *n == 0 once the range or the stream ends. */
const uint8_t *webp_cursor_refill(webp_cursor_t *c, size_t *n);

/* What a codec reports when its bits ran out: a file cut short, or a chunk
 * shorter than its contents. */
static inline imgf_err_t webp_eof_error(const webp_source_t *src) {
    return src->truncated ? IMGF_ERR_TRUNCATED : IMGF_ERR_DECODE;
}

typedef struct webp_vp8 webp_vp8_t;

imgf_err_t webp_vp8_open    (webp_vp8_t **out, webp_source_t *src, size_t pos, size_t len,
                             uint32_t caps);
int        webp_vp8_width   (const webp_vp8_t *d);
int        webp_vp8_height  (const webp_vp8_t *d);
imgf_err_t webp_vp8_next_row(webp_vp8_t *d, uint8_t *rgb);
void       webp_vp8_destroy (webp_vp8_t *d);

/* Lossless (VP8L) image, delivered as ARGB rows. `width` 0 reads the VP8L
 * header for the size; otherwise the stream is a headerless one of that size,
 * as an ALPH chunk carries. */
typedef struct webp_vp8l webp_vp8l_t;

imgf_err_t webp_vp8l_open     (webp_vp8l_t **out, webp_source_t *src, size_t pos, size_t len,
                               int width, int height, uint32_t caps);
int        webp_vp8l_width    (const webp_vp8l_t *d);
int        webp_vp8l_height   (const webp_vp8l_t *d);
bool       webp_vp8l_has_alpha(const webp_vp8l_t *d);
imgf_err_t webp_vp8l_next_row (webp_vp8l_t *d, uint32_t *argb);
void       webp_vp8l_destroy  (webp_vp8l_t *d);

#ifdef __cplusplus
}
#endif
