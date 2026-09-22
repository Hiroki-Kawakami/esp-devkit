/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "imgf_alloc.h"

#ifdef ESP_PLATFORM
#include "esp_heap_caps.h"
#else
#include <stdlib.h>
#endif

#ifdef ESP_PLATFORM
#define IMGF_DEFAULT_CAPS (MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT)

/* MALLOC_CAP_SIMD on its own, never OR-ed with INTERNAL: the P4 heap matches
 * caps one priority level at a time and lists the two on different levels for
 * the region most of the free internal memory lives in. Alone it still prefers
 * internal memory, and it keeps RTC RAM out — a PIE store there faults. */
#if defined(CONFIG_IDF_TARGET_ESP32P4)
#define IMGF_INTERNAL_CAPS MALLOC_CAP_SIMD
#else
#define IMGF_INTERNAL_CAPS (MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT)
#endif
#endif

void *imgf_alloc(size_t size, uint32_t caps) {
#ifdef ESP_PLATFORM
    return heap_caps_malloc(size, caps ? caps : IMGF_DEFAULT_CAPS);
#else
    (void)caps;
    return malloc(size);
#endif
}

void *imgf_alloc_internal(size_t size) {
#ifdef ESP_PLATFORM
    return heap_caps_malloc(size, IMGF_INTERNAL_CAPS);
#else
    return malloc(size);
#endif
}

void imgf_free(void *p) {
#ifdef ESP_PLATFORM
    heap_caps_free(p);
#else
    free(p);
#endif
}
