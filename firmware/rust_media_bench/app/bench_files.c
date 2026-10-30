/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Files in memory, so a demuxer's time is its own and not a card's.
 */

#include "bench_files.h"

#include <stdio.h>

#ifdef ESP_PLATFORM

#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include <sys/stat.h>

#include "esp_vfs.h"
#include "esp_vfs_ops.h"

#define MAX_FILES 12
#define MAX_OPEN 8
#define MOUNT "/mem"

typedef struct {
    char name[32];
    char path[40];
    const char *data;
    size_t size;
} file_t;

typedef struct {
    const file_t *file;
    off_t pos;
} handle_t;

static file_t s_files[MAX_FILES];
static handle_t s_handles[MAX_OPEN];
static int s_count;

static int mem_open(void *ctx, const char *path, int flags, int mode) {
    (void)ctx;
    (void)mode;
    if ((flags & O_ACCMODE) != O_RDONLY) {
        errno = EROFS;
        return -1;
    }
    for (int i = 0; i < s_count; i++) {
        if (strcmp(path + 1, s_files[i].name) != 0) continue;
        for (int fd = 0; fd < MAX_OPEN; fd++) {
            if (s_handles[fd].file) continue;
            s_handles[fd] = (handle_t){ &s_files[i], 0 };
            return fd;
        }
        errno = ENFILE;
        return -1;
    }
    errno = ENOENT;
    return -1;
}

static ssize_t mem_read(void *ctx, int fd, void *dst, size_t size) {
    (void)ctx;
    handle_t *h = &s_handles[fd];
    const off_t left = (off_t)h->file->size - h->pos;
    if (left <= 0) return 0;
    if ((off_t)size > left) size = (size_t)left;
    memcpy(dst, h->file->data + h->pos, size);
    h->pos += (off_t)size;
    return (ssize_t)size;
}

static off_t mem_lseek(void *ctx, int fd, off_t offset, int whence) {
    (void)ctx;
    handle_t *h = &s_handles[fd];
    if (whence == SEEK_SET) {
        h->pos = offset;
    } else if (whence == SEEK_CUR) {
        h->pos += offset;
    } else {
        h->pos = (off_t)h->file->size + offset;
    }
    return h->pos;
}

static int mem_fstat(void *ctx, int fd, struct stat *st) {
    (void)ctx;
    memset(st, 0, sizeof(*st));
    st->st_size = (off_t)s_handles[fd].file->size;
    st->st_mode = S_IFREG;
    return 0;
}

static int mem_close(void *ctx, int fd) {
    (void)ctx;
    s_handles[fd].file = NULL;
    return 0;
}

static const esp_vfs_fs_ops_t s_ops = {
    .open_p = mem_open,
    .read_p = mem_read,
    .lseek_p = mem_lseek,
    .fstat_p = mem_fstat,
    .close_p = mem_close,
};

const char *bench_file_add(const char *name, const void *data, size_t size) {
    if (s_count == 0 && esp_vfs_register_fs(MOUNT, &s_ops, ESP_VFS_FLAG_CONTEXT_PTR | ESP_VFS_FLAG_STATIC | ESP_VFS_FLAG_READONLY_FS, NULL) != ESP_OK) {
        return NULL;
    }
    if (s_count == MAX_FILES) return NULL;
    file_t *file = &s_files[s_count++];
    snprintf(file->name, sizeof(file->name), "%s", name);
    snprintf(file->path, sizeof(file->path), MOUNT "/%s", name);
    file->data = data;
    file->size = size;
    return file->path;
}

#else

#include <stdlib.h>

const char *bench_file_add(const char *name, const void *data, size_t size) {
    char *path = malloc(256);
    if (!path) return NULL;
    snprintf(path, 256, "/tmp/rust_media_bench_%s", name);
    FILE *f = fopen(path, "wb");
    if (!f) return NULL;
    const size_t written = fwrite(data, 1, size, f);
    fclose(f);
    return written == size ? path : NULL;
}

#endif
