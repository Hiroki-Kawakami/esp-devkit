/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Feeds a decompressor with input blocks. refill() returns a pointer to >= 1
 * input bytes and sets *n, or sets *n = 0 at end of input (return value may be
 * NULL in that case). Block-based so the inner decode loop reads through a
 * pointer instead of a callback per byte. */
typedef struct {
    const uint8_t *(*refill)(void *user, size_t *n);
    void *user;
} cmpf_src_t;

#ifdef __cplusplus
}
#endif
