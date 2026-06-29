/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Buffered reader on top of imgf_stream_t. Decoders pull one byte at a time;
 * batching the source through a 32KiB window keeps slow per-byte fread() off
 * the SD card path. Matches imgproc::InputStream's role.
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "imgf_stream.h"

#ifdef __cplusplus
extern "C" {
#endif

#define IMGF_BREADER_BUF_SIZE 32768

typedef struct {
    imgf_stream_t src;
    uint8_t      *buf;
    size_t        len;
    size_t        pos;
} imgf_breader_t;

bool   imgf_breader_init  (imgf_breader_t *br, imgf_stream_t src);
void   imgf_breader_deinit(imgf_breader_t *br);
size_t imgf_breader_read  (imgf_breader_t *br, void *dst, size_t n);
int    imgf_breader_byte  (imgf_breader_t *br);  /* -1 = EOF */

#ifdef __cplusplus
}
#endif
