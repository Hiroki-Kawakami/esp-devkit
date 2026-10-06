/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_alloc.h"

#include <stdlib.h>

#ifdef ESP_PLATFORM
#include "esp_heap_caps.h"
#endif

void *audf_malloc(size_t size, uint32_t caps) {
#ifdef ESP_PLATFORM
    if (caps) return heap_caps_malloc(size, caps);
#else
    (void)caps;
#endif
    return malloc(size);
}

void *audf_calloc(size_t count, size_t size, uint32_t caps) {
#ifdef ESP_PLATFORM
    if (caps) return heap_caps_calloc(count, size, caps);
#else
    (void)caps;
#endif
    return calloc(count, size);
}

void *audf_malloc_aligned(size_t align, size_t size, uint32_t caps) {
#ifdef ESP_PLATFORM
    return heap_caps_aligned_alloc(align, size, caps ? caps : MALLOC_CAP_DEFAULT);
#else
    (void)caps;
    return aligned_alloc(align, (size + align - 1) / align * align);
#endif
}

void audf_free(void *p) {
    free(p);
}
