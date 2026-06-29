/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdbool.h>
#include <stdint.h>

#include "imgf_stream.h"
#include "imgf_types.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Pure hints. The decoder may ignore any/all of them.
 * - target_w/h: decode-time downscale toward this size (JPEG picks a 1/N
 *   factor; PNG ignores it and decodes at full resolution).
 * - max_src_pixels: bound on the decoded geometry; if exceeded the decoder
 *   downscales further (0 = unbounded).
 * - alloc_caps: heap_caps_* bitmask for the large internal band buffer (0 =
 *   default). Ignored on host builds. */
typedef struct {
    uint16_t target_w;
    uint16_t target_h;
    uint32_t max_src_pixels;
    uint32_t alloc_caps;
} imgf_decode_opts_t;

typedef struct imgf_decoder imgf_decoder_t;

/* Parses the container header from `src` and prepares to deliver rows. On
 * success the width/height/pixfmt accessors are valid. `opts` may be NULL
 * (treated as all zeros). */
imgf_err_t     imgf_decoder_open       (imgf_decoder_t *dec, imgf_stream_t src,
                                        const imgf_decode_opts_t *opts);

/* Writes one decoded row to `dst` (width * channels bytes). Returns true while
 * rows remain; false at end-of-image or on error — distinguish via
 * imgf_decoder_last_error(). */
bool           imgf_decoder_next_row   (imgf_decoder_t *dec, uint8_t *dst);

uint16_t       imgf_decoder_width      (const imgf_decoder_t *dec);
uint16_t       imgf_decoder_height     (const imgf_decoder_t *dec);
imgf_pixfmt_t  imgf_decoder_pixfmt     (const imgf_decoder_t *dec);
imgf_err_t     imgf_decoder_last_error (const imgf_decoder_t *dec);

/* Safe on NULL. */
void           imgf_decoder_destroy    (imgf_decoder_t *dec);

#ifdef __cplusplus
}
#endif
