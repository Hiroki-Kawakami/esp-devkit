/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

#define MEDF_ARENA_ALIGNMENT 64
#define MEDF_READ_ALIGNMENT 64
#define MEDF_NO_REF 0

typedef struct {
    uint8_t *data;
    size_t size;
    /* Header and tag reads only: no read-ahead task, medf_buffer_read() is
       served from a window instead, and medf_buffer_view() always fails. */
    bool direct;
} medf_arena_t;

typedef struct medf_buffer medf_buffer_t;

size_t medf_arena_max_view(const medf_arena_t *arena);

medf_buffer_t *medf_buffer_open(const char *path, const medf_arena_t *arena);
void medf_buffer_close(medf_buffer_t *buffer);

off_t medf_buffer_size(const medf_buffer_t *buffer);
off_t medf_buffer_tell(const medf_buffer_t *buffer);
size_t medf_buffer_read(medf_buffer_t *buffer, void *out, size_t size);

/* Reads `size` bytes at `offset` into a fresh PSRAM allocation. FATFS copies
   the leading partial sector through its own window and then DMAs the rest
   straight to the caller, so the transfer only runs at full speed when the
   destination carries the same MEDF_READ_ALIGNMENT phase as the file offset
   (6.4 vs 11.3 MB/s measured on an SD card). The buffer is over-allocated
   to place the data at that phase: the return value is the data, *out_owner
   the pointer to heap_caps_free(). */
uint8_t *medf_buffer_read_alloc(medf_buffer_t *buffer, off_t offset, size_t size, void **out_owner);
off_t medf_buffer_seek(medf_buffer_t *buffer, off_t offset);
void medf_buffer_skip(medf_buffer_t *buffer, off_t delta);
void medf_buffer_set_readahead(medf_buffer_t *buffer, bool enabled);

const uint8_t *medf_buffer_view(medf_buffer_t *buffer, size_t size, uint32_t *ref);
const uint8_t *medf_buffer_view_at(medf_buffer_t *buffer, off_t offset, size_t size, uint32_t *ref);
void medf_buffer_release(medf_buffer_t *buffer, uint32_t ref);
void medf_buffer_release_all(medf_buffer_t *buffer);
void medf_buffer_interrupt(medf_buffer_t *buffer, bool interrupted);

#ifdef __cplusplus
}
#endif
