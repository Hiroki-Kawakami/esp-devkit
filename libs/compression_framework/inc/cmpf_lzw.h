/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Streaming variable-width LZW decompressor (12-bit codes at most), pull-based
 * like cmpf_inflate.
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "cmpf_source.h"

#ifdef __cplusplus
extern "C" {
#endif

#define CMPF_LZW_MAX_CODES 4096
#define CMPF_LZW_WORK_BYTES (CMPF_LZW_MAX_CODES * 4)

/* The two variants in use: GIF packs codes LSB-first and widens a code when
 * the next free entry reaches 1 << width; TIFF packs them MSB-first with
 * literal_bits 8 and widens one entry early. */
typedef struct {
    uint8_t literal_bits;
    uint8_t msb_first;
    uint8_t early_change;
} cmpf_lzw_opts_t;

typedef struct {
    cmpf_src_t src;
    const uint8_t *in_ptr;
    size_t   in_avail;
    uint32_t bitbuf;
    int      bitcnt;

    uint16_t *prefix;
    uint8_t  *suffix;
    uint8_t  *stack;
    uint16_t  stack_len;

    uint16_t clear;
    uint16_t next;
    uint16_t prev;
    uint8_t  prev_first;
    uint8_t  width;
    uint8_t  literal_bits;
    uint8_t  msb_first;
    uint8_t  early_change;

    uint8_t  ended;
    uint8_t  err;
} cmpf_lzw_t;

/* `work` is CMPF_LZW_WORK_BYTES owned by the caller, 2-byte aligned. Calling
 * init again with the same work area starts the next stream. literal_bits
 * outside 2..8 fails. Running out of input ends the stream like an
 * end-of-information code. */
bool   cmpf_lzw_init(cmpf_lzw_t *z, cmpf_src_t src, const cmpf_lzw_opts_t *opts, void *work);
size_t cmpf_lzw_read(cmpf_lzw_t *z, uint8_t *out, size_t n);

static inline bool cmpf_lzw_failed(const cmpf_lzw_t *z) { return z->err; }
static inline bool cmpf_lzw_ended (const cmpf_lzw_t *z) { return z->ended; }

#ifdef __cplusplus
}
#endif
