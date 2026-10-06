/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stddef.h>
#include <stdint.h>

/* caps 0 is plain malloc; on the host every call is plain malloc. */
void *audf_malloc(size_t size, uint32_t caps);
void *audf_calloc(size_t count, size_t size, uint32_t caps);
/* align is a power of two; free with audf_free. */
void *audf_malloc_aligned(size_t align, size_t size, uint32_t caps);
void  audf_free(void *p);
