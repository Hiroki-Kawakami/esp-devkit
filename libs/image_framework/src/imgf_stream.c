/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "imgf_stream.h"

#include <string.h>

static int buffer_read(void *user, void *dst, size_t n) {
    imgf_buffer_source_t *s = (imgf_buffer_source_t *)user;
    size_t avail = s->len - s->pos;
    size_t k = avail < n ? avail : n;
    if (k) memcpy(dst, s->data + s->pos, k);
    s->pos += k;
    return (int)k;
}

imgf_stream_t imgf_stream_from_buffer(imgf_buffer_source_t *state,
                                      const void *data, size_t len) {
    state->data = (const uint8_t *)data;
    state->len  = len;
    state->pos  = 0;
    imgf_stream_t s = { buffer_read, state };
    return s;
}

static int file_read(void *user, void *dst, size_t n) {
    imgf_file_source_t *s = (imgf_file_source_t *)user;
    if (!s->seeked) {
        if (s->offset > 0) fseek(s->fp, s->offset, SEEK_SET);
        s->seeked = 1;
    }
    if (s->limited) {
        if (s->remaining == 0) return 0;
        if (n > s->remaining) n = s->remaining;
    }
    size_t got = fread(dst, 1, n, s->fp);
    if (s->limited) s->remaining -= got;
    return (int)got;
}

imgf_stream_t imgf_stream_from_file(imgf_file_source_t *state,
                                    FILE *fp, long offset, size_t length) {
    state->fp        = fp;
    state->offset    = offset;
    state->remaining = length;
    state->limited   = length != 0 ? 1 : 0;
    state->seeked    = 0;
    imgf_stream_t s = { file_read, state };
    return s;
}
