/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Streaming DEFLATE (RFC 1951) decompressor — decompression only, pull-based.
 * read() yields up to n bytes and resumes mid-block on the next call. init()
 * consumes/validates the 2-byte zlib (RFC 1950) header.
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "cmpf_source.h"

#ifdef __cplusplus
extern "C" {
#endif

#define CMPF_INFLATE_WINDOW_BYTES 32768u

#define CMPF_INFLATE_FAST_BITS 9
#define CMPF_INFLATE_FAST_SIZE (1 << CMPF_INFLATE_FAST_BITS)

typedef struct {
    int16_t  count[16];
    int16_t  symbol[288];
    uint16_t fast[CMPF_INFLATE_FAST_SIZE];
} cmpf_inflate_huff_t;

typedef struct {
    cmpf_src_t src;
    const uint8_t *in_ptr;
    size_t   in_avail;
    uint32_t bitbuf;
    int      bitcnt;
    uint8_t  eof;

    uint8_t *win;
    uint32_t wpos;

    cmpf_inflate_huff_t lit;
    cmpf_inflate_huff_t dist;

    uint8_t  block_active;
    int      bfinal;
    int      mode;
    uint32_t stored_rem;
    int      copy_rem;
    int      copy_dist;

    uint8_t  ended;
    uint8_t  err;
} cmpf_inflate_t;

/* `window` is CMPF_INFLATE_WINDOW_BYTES owned by the caller. Calling init
 * again with the same window starts the next stream without reallocating. */
bool   cmpf_inflate_init(cmpf_inflate_t *z, cmpf_src_t src, uint8_t *window);
size_t cmpf_inflate_read(cmpf_inflate_t *z, uint8_t *out, size_t n);

static inline bool cmpf_inflate_failed(const cmpf_inflate_t *z) { return z->err; }
static inline bool cmpf_inflate_ended (const cmpf_inflate_t *z) { return z->ended; }

#ifdef __cplusplus
}
#endif
