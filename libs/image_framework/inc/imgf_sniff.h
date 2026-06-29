/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stddef.h>

#include "imgf_decoder.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    IMGF_FMT_UNKNOWN,
    IMGF_FMT_JPEG,
    IMGF_FMT_PNG,
} imgf_format_t;

/* Inspects the first up-to-8 bytes of a container header. The stream itself
 * does not have peek; the caller decides how to look ahead (read into a small
 * buffer, then rewind for FILE*; or just inspect the head of a memory blob). */
imgf_format_t   imgf_sniff       (const void *header, size_t len);

/* Allocates the decoder matching `fmt`. Returns NULL for IMGF_FMT_UNKNOWN. */
imgf_decoder_t *imgf_make_decoder(imgf_format_t fmt);

#ifdef __cplusplus
}
#endif
