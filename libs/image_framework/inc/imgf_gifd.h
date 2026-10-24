/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include "imgf_decoder.h"

#ifdef __cplusplus
extern "C" {
#endif

/* GIF decoder for the first frame only, at the logical screen size (grown to
 * cover the frame if the screen is smaller). Always RGB888: the transparent
 * colour and everything outside the frame are white. An interlaced frame is
 * expanded whole on the first row inside it, into one byte per pixel out of
 * `alloc_caps`. */
imgf_decoder_t *imgf_gifd_create(void);

#ifdef __cplusplus
}
#endif
