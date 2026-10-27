#include "esp_heap_caps.h"
#include <stdlib.h>
#include <string.h>
#if defined(__APPLE__)
#include <malloc/malloc.h>
#elif defined(__GLIBC__)
#include <malloc.h>
#endif

void *heap_caps_malloc(size_t size, uint32_t caps) {
    (void)caps;
    return malloc(size);
}

void *heap_caps_calloc(size_t n, size_t size, uint32_t caps) {
    (void)caps;
    return calloc(n, size);
}

void *heap_caps_realloc(void *ptr, size_t size, uint32_t caps) {
    (void)caps;
    return realloc(ptr, size);
}

void *heap_caps_aligned_alloc(size_t alignment, size_t size, uint32_t caps) {
    (void)caps;
    // posix_memalign requires alignment to be a power-of-two multiple of
    // sizeof(void*); ESP-IDF allows smaller (e.g. 4). Round up so a valid IDF
    // request (4-byte align of an L8 framebuffer) doesn't fail on the host.
    if (alignment < sizeof(void *)) alignment = sizeof(void *);
    void *p = NULL;
    if (posix_memalign(&p, alignment, size) != 0) return NULL;
    return p;  // freeable with heap_caps_free()/free()
}

void *heap_caps_aligned_calloc(size_t alignment, size_t n, size_t size, uint32_t caps) {
    if (size && n > SIZE_MAX / size) return NULL;
    void *p = heap_caps_aligned_alloc(alignment, n * size, caps);
    if (p) memset(p, 0, n * size);
    return p;
}

void heap_caps_free(void *ptr) {
    free(ptr);
}

#define HOST_HEAP_SIZE ((size_t)256 * 1024 * 1024)

size_t heap_caps_get_free_size(uint32_t caps) {
    (void)caps;
    return HOST_HEAP_SIZE;
}

size_t heap_caps_get_minimum_free_size(uint32_t caps) {
    (void)caps;
    return HOST_HEAP_SIZE;
}

size_t heap_caps_get_largest_free_block(uint32_t caps) {
    (void)caps;
    return HOST_HEAP_SIZE;
}

size_t heap_caps_get_total_size(uint32_t caps) {
    (void)caps;
    return HOST_HEAP_SIZE;
}

void heap_caps_get_info(multi_heap_info_t *info, uint32_t caps) {
    (void)caps;
    memset(info, 0, sizeof(*info));
    info->total_free_bytes = HOST_HEAP_SIZE;
    info->largest_free_block = HOST_HEAP_SIZE;
    info->minimum_free_bytes = HOST_HEAP_SIZE;
}

size_t heap_caps_get_containing_block_size(void *ptr) {
    if (!ptr) return 0;
#if defined(__APPLE__)
    return malloc_size(ptr);
#elif defined(__GLIBC__)
    return malloc_usable_size(ptr);
#else
    (void)ptr;
    return 0;
#endif
}
