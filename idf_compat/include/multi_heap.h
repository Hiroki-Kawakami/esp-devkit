#pragma once
// Host counterpart of ESP-IDF <multi_heap.h>: a heap carved out of a region the
// caller owns. Blocks really come from inside the region, so a full heap fails
// and an address-range test tells its blocks apart, as on the device. First fit
// rather than TLSF, and like the IDF API it takes no lock of its own.
#include <stdbool.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct multi_heap_info *multi_heap_handle_t;

typedef struct {
    size_t total_free_bytes;
    size_t total_allocated_bytes;
    size_t largest_free_block;
    size_t minimum_free_bytes;
    size_t allocated_blocks;
    size_t free_blocks;
    size_t total_blocks;
} multi_heap_info_t;

multi_heap_handle_t multi_heap_register(void *start, size_t size);
void *multi_heap_malloc(multi_heap_handle_t heap, size_t size);
void *multi_heap_aligned_alloc(multi_heap_handle_t heap, size_t size, size_t alignment);
void multi_heap_free(multi_heap_handle_t heap, void *p);
size_t multi_heap_free_size(multi_heap_handle_t heap);
void multi_heap_get_info(multi_heap_handle_t heap, multi_heap_info_t *info);

#ifdef __cplusplus
}
#endif
