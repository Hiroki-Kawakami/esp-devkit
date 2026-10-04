/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_convert.h"
#include <string.h>
#include "audf_sample.h"

static size_t pcm_bytes(audf_pcm_t pcm) {
    switch (pcm) {
        case AUDF_PCM_S24: return 3;
        case AUDF_PCM_S32: return 4;
        default:           return 2;
    }
}

static int32_t load_pcm(audf_pcm_t pcm, const uint8_t *p) {
    switch (pcm) {
        case AUDF_PCM_S24: {
            int32_t v = (int32_t)((uint32_t)p[0] << 8 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 24);
            return v >> 8;
        }
        case AUDF_PCM_S32: {
            int32_t v = (int32_t)((uint32_t)p[0] | (uint32_t)p[1] << 8 |
                                  (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24);
            return v >> 8;
        }
        default:
            return (int32_t)(int16_t)((uint16_t)p[0] | (uint16_t)p[1] << 8) * (1 << AUDF_S16_SHIFT);
    }
}

static void store_pcm(audf_pcm_t pcm, uint8_t *p, int32_t v) {
    if (pcm == AUDF_PCM_S16) {
        int64_t s = ((int64_t)v + (1 << (AUDF_S16_SHIFT - 1))) >> AUDF_S16_SHIFT;
        if (s > INT16_MAX) s = INT16_MAX;
        if (s < INT16_MIN) s = INT16_MIN;
        p[0] = (uint8_t)s;
        p[1] = (uint8_t)(s >> 8);
        return;
    }
    if (v > AUDF_S24_MAX) v = AUDF_S24_MAX;
    if (v < AUDF_S24_MIN) v = AUDF_S24_MIN;
    if (pcm == AUDF_PCM_S24) {
        p[0] = (uint8_t)v;
        p[1] = (uint8_t)(v >> 8);
        p[2] = (uint8_t)(v >> 16);
    } else {
        uint32_t u = (uint32_t)v << 8;
        p[0] = (uint8_t)u;
        p[1] = (uint8_t)(u >> 8);
        p[2] = (uint8_t)(u >> 16);
        p[3] = (uint8_t)(u >> 24);
    }
}

/* Walk backwards when widening so src == dst works in place. */
void audf_convert_from_pcm(const void *src, audf_pcm_t pcm, void *dst, audf_fmt_t fmt, size_t samples) {
    const uint8_t *s = src;
    size_t sb = pcm_bytes(pcm);
    if (audf_fmt_bytes(fmt) > sb) {
        for (size_t i = samples; i-- > 0;) audf_store(fmt, dst, i, load_pcm(pcm, s + i * sb));
    } else {
        for (size_t i = 0; i < samples; i++) audf_store(fmt, dst, i, load_pcm(pcm, s + i * sb));
    }
}

void audf_convert_to_pcm(const void *src, audf_fmt_t fmt, void *dst, audf_pcm_t pcm, size_t samples) {
    uint8_t *d = dst;
    size_t db = pcm_bytes(pcm);
    if (db > audf_fmt_bytes(fmt)) {
        for (size_t i = samples; i-- > 0;) store_pcm(pcm, d + i * db, audf_load(fmt, src, i));
    } else {
        for (size_t i = 0; i < samples; i++) store_pcm(pcm, d + i * db, audf_load(fmt, src, i));
    }
}

void audf_convert(const void *src, audf_fmt_t src_fmt, void *dst, audf_fmt_t dst_fmt, size_t samples) {
    if (src_fmt == dst_fmt) {
        if (src != dst) memmove(dst, src, samples * audf_fmt_bytes(src_fmt));
        return;
    }
    if (dst_fmt == AUDF_FMT_S32) {
        for (size_t i = samples; i-- > 0;) audf_store(dst_fmt, dst, i, audf_load(src_fmt, src, i));
    } else {
        for (size_t i = 0; i < samples; i++) audf_store(dst_fmt, dst, i, audf_load(src_fmt, src, i));
    }
}
