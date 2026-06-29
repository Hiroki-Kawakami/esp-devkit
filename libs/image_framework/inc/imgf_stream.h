/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Sequential byte source consumed by the decoders. read() returns the number of
 * bytes copied (0 = EOF, < 0 = source error). The decoders pull one byte at a
 * time but wrap this in an internal 32KiB window, so a from_file source only
 * sees batched fread() calls. */
typedef struct imgf_stream {
    int   (*read)(void *user, void *dst, size_t n);
    void   *user;
} imgf_stream_t;

/* Convenience wrappers. The state struct must outlive the returned stream (it
 * is referenced through the user pointer). Typical use: declare on the stack
 * next to the decoder. */

typedef struct {
    const uint8_t *data;
    size_t         len;
    size_t         pos;
} imgf_buffer_source_t;

imgf_stream_t imgf_stream_from_buffer(imgf_buffer_source_t *state,
                                      const void *data, size_t len);

/* Reads from `fp` starting at `offset` for `length` bytes (length == 0 means
 * to EOF). The seek to `offset` happens lazily on the first read. The caller
 * owns the FILE*. */
typedef struct {
    FILE   *fp;
    long    offset;
    size_t  remaining;
    uint8_t limited;
    uint8_t seeked;
} imgf_file_source_t;

imgf_stream_t imgf_stream_from_file(imgf_file_source_t *state,
                                    FILE *fp, long offset, size_t length);

#ifdef __cplusplus
}
#endif
