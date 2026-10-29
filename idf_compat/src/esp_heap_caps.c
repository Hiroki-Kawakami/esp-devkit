#include "esp_heap_caps.h"
#include "multi_heap.h"
#include "simulator/sim_psram.h"
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#if defined(__APPLE__)
#include <malloc/malloc.h>
#elif defined(__GLIBC__)
#include <malloc.h>
#endif

#define HOST_HEAP_SIZE ((size_t)256 * 1024 * 1024)

static pthread_once_t   s_psram_once = PTHREAD_ONCE_INIT;
static pthread_mutex_t  s_psram_lock = PTHREAD_MUTEX_INITIALIZER;
static uint8_t         *s_psram_base;
static size_t           s_psram_size;
static multi_heap_handle_t s_psram_heap;

static void psram_init(void) {
    const char *path = getenv("SIM_PSRAM_PATH");
    const char *size = getenv("SIM_PSRAM_SIZE");
    if (!path || !size) return;
    size_t bytes = strtoull(size, NULL, 0);
    int fd = open(path, O_RDWR);
    if (fd < 0) {
        perror("sim psram: open");
        return;
    }
    void *base = mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (base == MAP_FAILED) {
        perror("sim psram: mmap");
        return;
    }
    s_psram_heap = multi_heap_register(base, bytes);
    if (!s_psram_heap) {
        fprintf(stderr, "sim psram: region too small\n");
        munmap(base, bytes);
        return;
    }
    s_psram_base = base;
    s_psram_size = bytes;
}

static multi_heap_handle_t psram_heap(void) {
    pthread_once(&s_psram_once, psram_init);
    return s_psram_heap;
}

static bool in_psram(const void *ptr) {
    return s_psram_base && (const uint8_t *)ptr >= s_psram_base &&
           (const uint8_t *)ptr < s_psram_base + s_psram_size;
}

bool sim_psram_region(void **base, size_t *size) {
    if (!psram_heap()) return false;
    *base = s_psram_base;
    *size = s_psram_size;
    return true;
}

static void *psram_alloc(size_t alignment, size_t size) {
    pthread_mutex_lock(&s_psram_lock);
    void *p = multi_heap_aligned_alloc(s_psram_heap, size, alignment);
    pthread_mutex_unlock(&s_psram_lock);
    return p;
}

void *heap_caps_aligned_alloc(size_t alignment, size_t size, uint32_t caps) {
    if ((caps & MALLOC_CAP_SPIRAM) && psram_heap()) return psram_alloc(alignment, size);
    // posix_memalign requires alignment to be a power-of-two multiple of
    // sizeof(void*); ESP-IDF allows smaller (e.g. 4). Round up so a valid IDF
    // request (4-byte align of an L8 framebuffer) doesn't fail on the host.
    if (alignment < sizeof(void *)) alignment = sizeof(void *);
    void *p = NULL;
    if (posix_memalign(&p, alignment, size) != 0) return NULL;
    return p;  // freeable with heap_caps_free()/free()
}

void *heap_caps_malloc(size_t size, uint32_t caps) {
    if ((caps & MALLOC_CAP_SPIRAM) && psram_heap()) return psram_alloc(sizeof(void *), size);
    return malloc(size);
}

void *heap_caps_calloc(size_t n, size_t size, uint32_t caps) {
    if (size && n > SIZE_MAX / size) return NULL;
    void *p = heap_caps_malloc(n * size, caps);
    if (p) memset(p, 0, n * size);
    return p;
}

void heap_caps_free(void *ptr) {
    if (in_psram(ptr)) {
        pthread_mutex_lock(&s_psram_lock);
        multi_heap_free(s_psram_heap, ptr);
        pthread_mutex_unlock(&s_psram_lock);
        return;
    }
    free(ptr);
}

void *heap_caps_realloc(void *ptr, size_t size, uint32_t caps) {
    if (!in_psram(ptr) && !((caps & MALLOC_CAP_SPIRAM) && psram_heap())) return realloc(ptr, size);
    void *p = heap_caps_malloc(size, caps);
    if (!p || !ptr) return p;
    size_t old = heap_caps_get_containing_block_size(ptr);
    memcpy(p, ptr, old < size ? old : size);
    heap_caps_free(ptr);
    return p;
}

void *heap_caps_aligned_calloc(size_t alignment, size_t n, size_t size, uint32_t caps) {
    if (size && n > SIZE_MAX / size) return NULL;
    void *p = heap_caps_aligned_alloc(alignment, n * size, caps);
    if (p) memset(p, 0, n * size);
    return p;
}

static size_t psram_free_size(void) {
    pthread_mutex_lock(&s_psram_lock);
    size_t n = multi_heap_free_size(s_psram_heap);
    pthread_mutex_unlock(&s_psram_lock);
    return n;
}

size_t heap_caps_get_free_size(uint32_t caps) {
    if (caps == MALLOC_CAP_SPIRAM && psram_heap()) return psram_free_size();
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
    if (caps == MALLOC_CAP_SPIRAM && psram_heap()) return s_psram_size;
    return HOST_HEAP_SIZE;
}

void heap_caps_get_info(multi_heap_info_t *info, uint32_t caps) {
    if (caps == MALLOC_CAP_SPIRAM && psram_heap()) {
        pthread_mutex_lock(&s_psram_lock);
        multi_heap_get_info(s_psram_heap, info);
        pthread_mutex_unlock(&s_psram_lock);
        return;
    }
    memset(info, 0, sizeof(*info));
    info->total_free_bytes = HOST_HEAP_SIZE;
    info->largest_free_block = HOST_HEAP_SIZE;
    info->minimum_free_bytes = HOST_HEAP_SIZE;
}

size_t heap_caps_get_containing_block_size(void *ptr) {
    if (!ptr) return 0;
    if (in_psram(ptr)) {
        pthread_mutex_lock(&s_psram_lock);
        size_t n = multi_heap_get_allocated_size(s_psram_heap, ptr);
        pthread_mutex_unlock(&s_psram_lock);
        return n;
    }
#if defined(__APPLE__)
    return malloc_size(ptr);
#elif defined(__GLIBC__)
    return malloc_usable_size(ptr);
#else
    return 0;
#endif
}
