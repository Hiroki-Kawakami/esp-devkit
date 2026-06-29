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

#ifdef __cplusplus
extern "C" {
#endif

/* The byte source feeds the inflater with input blocks. refill() returns a
 * pointer to >= 1 input bytes and sets *n, or sets *n = 0 at end of input
 * (return value may be NULL in that case). Block-based so the inner decode
 * loop reads through a pointer instead of a callback per byte. */
typedef struct {
    const uint8_t *(*refill)(void *user, size_t *n);
    void *user;
} imgf_inflate_src_t;

#define IMGF_INFLATE_FAST_BITS 9
#define IMGF_INFLATE_FAST_SIZE (1 << IMGF_INFLATE_FAST_BITS)

typedef struct {
    int16_t  count[16];
    int16_t  symbol[288];
    uint16_t fast[IMGF_INFLATE_FAST_SIZE];
} imgf_inflate_huff_t;

typedef struct {
    imgf_inflate_src_t src;
    const uint8_t *in_ptr;
    size_t   in_avail;
    uint32_t bitbuf;
    int      bitcnt;
    uint8_t  eof;

    uint8_t *win;
    uint32_t wpos;

    imgf_inflate_huff_t lit;
    imgf_inflate_huff_t dist;

    uint8_t  block_active;
    int      bfinal;
    int      mode;
    uint32_t stored_rem;
    int      copy_rem;
    int      copy_dist;

    uint8_t  ended;
    uint8_t  err;
} imgf_inflate_t;

bool   imgf_inflate_init  (imgf_inflate_t *z, imgf_inflate_src_t src);
size_t imgf_inflate_read  (imgf_inflate_t *z, uint8_t *out, size_t n);
void   imgf_inflate_deinit(imgf_inflate_t *z);

static inline bool imgf_inflate_failed(const imgf_inflate_t *z) { return z->err; }
static inline bool imgf_inflate_ended (const imgf_inflate_t *z) { return z->ended; }

#ifdef __cplusplus
}
#endif
