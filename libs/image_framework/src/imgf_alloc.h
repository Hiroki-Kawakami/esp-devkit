/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Allocation shim: on ESP-IDF prefers caller-supplied heap caps (falls back to
 * the SPIRAM default); on the host just wraps malloc/free.
 */

#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

void *imgf_alloc          (size_t size, uint32_t caps);
void *imgf_alloc_internal (size_t size);
void  imgf_free           (void *p);

#ifdef __cplusplus
}
#endif
