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

void audf_free(void *p) {
    free(p);
}
