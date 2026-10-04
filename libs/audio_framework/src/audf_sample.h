/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdint.h>

#include "audf_types.h"

#define AUDF_INLINE static inline __attribute__((always_inline))

#define AUDF_S16_SHIFT 8
#define AUDF_S24_MAX   ((1 << 23) - 1)
#define AUDF_S24_MIN   (-(1 << 23))

AUDF_INLINE int32_t audf_sat32(int64_t v) {
    if (v > INT32_MAX) return INT32_MAX;
    if (v < INT32_MIN) return INT32_MIN;
    return (int32_t)v;
}

AUDF_INLINE int32_t audf_load(audf_fmt_t fmt, const void *buf, size_t i) {
    if (fmt == AUDF_FMT_S16) return (int32_t)((const int16_t *)buf)[i] * (1 << AUDF_S16_SHIFT);
    return ((const int32_t *)buf)[i];
}

AUDF_INLINE void audf_store(audf_fmt_t fmt, void *buf, size_t i, int64_t v) {
    if (fmt == AUDF_FMT_S16) {
        v = (v + (1 << (AUDF_S16_SHIFT - 1))) >> AUDF_S16_SHIFT;
        if (v > INT16_MAX) v = INT16_MAX;
        if (v < INT16_MIN) v = INT16_MIN;
        ((int16_t *)buf)[i] = (int16_t)v;
    } else {
        ((int32_t *)buf)[i] = audf_sat32(v);
    }
}
