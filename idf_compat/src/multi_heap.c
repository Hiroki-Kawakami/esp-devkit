#include "multi_heap.h"
#include <stdint.h>
#include <string.h>

typedef struct block {
    struct block *next;
    size_t size;
    bool free;
} block_t;

struct multi_heap_info {
    block_t *first;
    size_t free_bytes;
    size_t minimum_free_bytes;
};

#define ALIGN_UP(value, align) (((value) + (align) - 1) & ~((uintptr_t)(align) - 1))
#define HEADER ALIGN_UP(sizeof(block_t), sizeof(void *))
#define SPLIT_MIN (HEADER + 32)

multi_heap_handle_t multi_heap_register(void *start, size_t size) {
    uintptr_t at = ALIGN_UP((uintptr_t)start, sizeof(void *));
    const uintptr_t end = (uintptr_t)start + size;
    if (at + sizeof(struct multi_heap_info) + SPLIT_MIN > end) return NULL;
    struct multi_heap_info *heap = (struct multi_heap_info *)at;
    at = ALIGN_UP(at + sizeof(*heap), sizeof(void *));
    block_t *first = (block_t *)at;
    first->next = NULL;
    first->size = end - at - HEADER;
    first->free = true;
    heap->first = first;
    heap->free_bytes = first->size;
    heap->minimum_free_bytes = first->size;
    return heap;
}

static uint8_t *payload(block_t *block) {
    return (uint8_t *)block + HEADER;
}

void *multi_heap_aligned_alloc(multi_heap_handle_t heap, size_t size, size_t alignment) {
    if (!heap || size == 0) return NULL;
    if (alignment < sizeof(void *)) alignment = sizeof(void *);
    for (block_t *block = heap->first; block; block = block->next) {
        if (!block->free) continue;
        const uintptr_t start = (uintptr_t)payload(block);
        const uintptr_t user = ALIGN_UP(start + sizeof(void *), alignment);
        const size_t need = (user - start) + ALIGN_UP(size, sizeof(void *));
        if (need > block->size) continue;
        if (block->size - need >= SPLIT_MIN) {
            block_t *rest = (block_t *)(start + need);
            rest->next = block->next;
            rest->size = block->size - need - HEADER;
            rest->free = true;
            block->next = rest;
            block->size = need;
            heap->free_bytes -= HEADER;
        }
        block->free = false;
        heap->free_bytes -= block->size;
        if (heap->free_bytes < heap->minimum_free_bytes) {
            heap->minimum_free_bytes = heap->free_bytes;
        }
        ((block_t **)user)[-1] = block;
        return (void *)user;
    }
    return NULL;
}

void *multi_heap_malloc(multi_heap_handle_t heap, size_t size) {
    return multi_heap_aligned_alloc(heap, size, sizeof(void *));
}

void multi_heap_free(multi_heap_handle_t heap, void *p) {
    if (!heap || !p) return;
    block_t *block = ((block_t **)p)[-1];
    block->free = true;
    heap->free_bytes += block->size;
    for (block_t *it = heap->first; it;) {
        if (it->free && it->next && it->next->free) {
            heap->free_bytes += HEADER;
            it->size += HEADER + it->next->size;
            it->next = it->next->next;
            continue;
        }
        it = it->next;
    }
}

size_t multi_heap_free_size(multi_heap_handle_t heap) {
    return heap ? heap->free_bytes : 0;
}

void multi_heap_get_info(multi_heap_handle_t heap, multi_heap_info_t *info) {
    memset(info, 0, sizeof(*info));
    if (!heap) return;
    info->total_free_bytes = heap->free_bytes;
    info->minimum_free_bytes = heap->minimum_free_bytes;
    for (block_t *block = heap->first; block; block = block->next) {
        info->total_blocks++;
        if (block->free) {
            info->free_blocks++;
            if (block->size > info->largest_free_block) info->largest_free_block = block->size;
        } else {
            info->allocated_blocks++;
            info->total_allocated_bytes += block->size;
        }
    }
}
