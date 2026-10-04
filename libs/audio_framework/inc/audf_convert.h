/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stddef.h>

#include "audf_types.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Interchange layouts at the edges of a chain, little-endian. S24 is packed
 * 3-byte; S32 is full-range, which also covers 24-bit MSB-justified in 32. */
typedef enum {
    AUDF_PCM_S16 = 0,
    AUDF_PCM_S24,
    AUDF_PCM_S32,
} audf_pcm_t;

/* src and dst may be the same buffer. Narrowing saturates. */
void audf_convert_from_pcm(const void *src, audf_pcm_t pcm, void *dst, audf_fmt_t fmt, size_t samples);
void audf_convert_to_pcm(const void *src, audf_fmt_t fmt, void *dst, audf_pcm_t pcm, size_t samples);
void audf_convert(const void *src, audf_fmt_t src_fmt, void *dst, audf_fmt_t dst_fmt, size_t samples);

#ifdef __cplusplus
}
#endif
