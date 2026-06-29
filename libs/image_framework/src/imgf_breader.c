/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "imgf_breader.h"

#include <string.h>

#include "imgf_alloc.h"

bool imgf_breader_init(imgf_breader_t *br, imgf_stream_t src) {
    br->src = src;
    br->buf = (uint8_t *)imgf_alloc(IMGF_BREADER_BUF_SIZE, 0);
    br->len = 0;
    br->pos = 0;
    return br->buf != NULL;
}

void imgf_breader_deinit(imgf_breader_t *br) {
    if (br->buf) {
        imgf_free(br->buf);
        br->buf = NULL;
    }
}

static size_t source_read(imgf_breader_t *br, void *dst, size_t n) {
    if (!br->src.read) return 0;
    int got = br->src.read(br->src.user, dst, n);
    return got > 0 ? (size_t)got : 0;
}

size_t imgf_breader_read(imgf_breader_t *br, void *dst, size_t n) {
    uint8_t *d = (uint8_t *)dst;
    size_t total = 0;

    while (n) {
        size_t avail = br->len - br->pos;
        if (avail == 0) {
            br->pos = 0;
            br->len = source_read(br, br->buf, IMGF_BREADER_BUF_SIZE);
            if (br->len == 0) break;
            avail = br->len;
        }
        size_t k = avail < n ? avail : n;
        memcpy(d, br->buf + br->pos, k);
        br->pos += k;
        d += k;
        total += k;
        n -= k;
    }
    return total;
}

int imgf_breader_byte(imgf_breader_t *br) {
    if (br->pos >= br->len) {
        br->pos = 0;
        br->len = source_read(br, br->buf, IMGF_BREADER_BUF_SIZE);
        if (br->len == 0) return -1;
    }
    return br->buf[br->pos++];
}
