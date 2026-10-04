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

/* Sample formats between modules, interleaved. S32 is 24-bit PCM (full scale
 * 1 << 23) in an int32, so 8 bits of headroom survive a module boundary; S16
 * saturates at every boundary. */
typedef enum {
    AUDF_FMT_S16 = 0,
    AUDF_FMT_S32,
} audf_fmt_t;

#define AUDF_MAX_CHANNELS 8

static inline size_t audf_fmt_bytes(audf_fmt_t fmt) {
    return fmt == AUDF_FMT_S32 ? 4 : 2;
}

static inline size_t audf_frame_bytes(audf_fmt_t fmt, uint8_t channels) {
    return audf_fmt_bytes(fmt) * channels;
}

#ifdef __cplusplus
}
#endif
