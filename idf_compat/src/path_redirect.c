/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * The libc entry points below are defined in the executable, so statically
 * linked calls resolve here, translate the path, and forward to the real libc
 * via dlsym(RTLD_NEXT).
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE  /* RTLD_NEXT on glibc */
#endif

#include "simulator/path_redirect.h"

#include <dirent.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>

#define REDIRECT_MAX    4
#define MOUNT_POINT_MAX 64
#define HOST_DIR_MAX    1024
#define PATH_BUF_MAX    (HOST_DIR_MAX + 256)

typedef struct {
    char   mount_point[MOUNT_POINT_MAX];
    size_t mount_len;  /* 0 = free slot */
    char   host_dir[HOST_DIR_MAX];
} redirect_entry_t;

static redirect_entry_t g_entries[REDIRECT_MAX];
static pthread_mutex_t  g_lock = PTHREAD_MUTEX_INITIALIZER;

static redirect_entry_t *find_locked(const char *mount_point) {
    for (int i = 0; i < REDIRECT_MAX; i++) {
        if (g_entries[i].mount_len > 0 && strcmp(g_entries[i].mount_point, mount_point) == 0) {
            return &g_entries[i];
        }
    }
    return NULL;
}

esp_err_t sim_path_redirect_add(const char *mount_point, const char *host_dir) {
    if (!mount_point || !host_dir) return ESP_ERR_INVALID_ARG;
    const size_t mount_len = strlen(mount_point);
    if (mount_len == 0 || mount_len >= MOUNT_POINT_MAX || strlen(host_dir) >= HOST_DIR_MAX) {
        return ESP_ERR_INVALID_ARG;
    }
    esp_err_t err = ESP_ERR_NO_MEM;
    pthread_mutex_lock(&g_lock);
    if (find_locked(mount_point)) {
        err = ESP_ERR_INVALID_STATE;
    } else {
        for (int i = 0; i < REDIRECT_MAX; i++) {
            redirect_entry_t *entry = &g_entries[i];
            if (entry->mount_len > 0) continue;
            strcpy(entry->mount_point, mount_point);
            strcpy(entry->host_dir, host_dir);
            entry->mount_len = mount_len;
            err = ESP_OK;
            break;
        }
    }
    pthread_mutex_unlock(&g_lock);
    if (err == ESP_OK) {
        fprintf(stderr, "I (SIM_REDIRECT) %s -> %s\n", mount_point, host_dir);
    }
    return err;
}

esp_err_t sim_path_redirect_remove(const char *mount_point) {
    if (!mount_point) return ESP_ERR_INVALID_ARG;
    pthread_mutex_lock(&g_lock);
    redirect_entry_t *entry = find_locked(mount_point);
    if (entry) entry->mount_len = 0;
    pthread_mutex_unlock(&g_lock);
    return entry ? ESP_OK : ESP_ERR_INVALID_STATE;
}

static const char *redirect(const char *path, char *buf, size_t buf_sz) {
    if (!path) return path;
    const char *result = path;
    pthread_mutex_lock(&g_lock);
    for (int i = 0; i < REDIRECT_MAX; i++) {
        const redirect_entry_t *entry = &g_entries[i];
        if (entry->mount_len == 0) continue;
        if (strncmp(path, entry->mount_point, entry->mount_len) != 0) continue;
        const char trail = path[entry->mount_len];
        if (trail != '\0' && trail != '/') continue;
        snprintf(buf, buf_sz, "%s%s", entry->host_dir, path + entry->mount_len);
        result = buf;
        break;
    }
    pthread_mutex_unlock(&g_lock);
    return result;
}

DIR *opendir(const char *name) {
    static DIR *(*real)(const char *) = NULL;
    if (!real) real = (DIR *(*)(const char *))dlsym(RTLD_NEXT, "opendir");
    char buf[PATH_BUF_MAX];
    return real(redirect(name, buf, sizeof(buf)));
}

int open(const char *pathname, int flags, ...) {
    static int (*real)(const char *, int, ...) = NULL;
    if (!real) real = (int (*)(const char *, int, ...))dlsym(RTLD_NEXT, "open");
    char buf[PATH_BUF_MAX];
    const char *p = redirect(pathname, buf, sizeof(buf));
    if (flags & O_CREAT) {
        va_list ap;
        va_start(ap, flags);
        mode_t mode = (mode_t)va_arg(ap, int);
        va_end(ap);
        return real(p, flags, mode);
    }
    return real(p, flags);
}

FILE *fopen(const char *pathname, const char *mode) {
    static FILE *(*real)(const char *, const char *) = NULL;
    if (!real) real = (FILE *(*)(const char *, const char *))dlsym(RTLD_NEXT, "fopen");
    char buf[PATH_BUF_MAX];
    return real(redirect(pathname, buf, sizeof(buf)), mode);
}

int stat(const char *pathname, struct stat *st) {
    static int (*real)(const char *, struct stat *) = NULL;
    if (!real) real = (int (*)(const char *, struct stat *))dlsym(RTLD_NEXT, "stat");
    char buf[PATH_BUF_MAX];
    return real(redirect(pathname, buf, sizeof(buf)), st);
}

int rename(const char *oldpath, const char *newpath) {
    static int (*real)(const char *, const char *) = NULL;
    if (!real) real = (int (*)(const char *, const char *))dlsym(RTLD_NEXT, "rename");
    char obuf[PATH_BUF_MAX], nbuf[PATH_BUF_MAX];
    return real(redirect(oldpath, obuf, sizeof(obuf)),
                redirect(newpath, nbuf, sizeof(nbuf)));
}

int unlink(const char *pathname) {
    static int (*real)(const char *) = NULL;
    if (!real) real = (int (*)(const char *))dlsym(RTLD_NEXT, "unlink");
    char buf[PATH_BUF_MAX];
    return real(redirect(pathname, buf, sizeof(buf)));
}
