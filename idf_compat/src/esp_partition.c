// Host implementation of the ESP-IDF partition API (esp_partition.h): each
// partition is a file read into memory once and written back on every change.

#include "esp_partition.h"

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define PARTITION_MAX 8
#define MAP_MAX 16
#define SECTOR 4096

typedef struct {
    esp_partition_t info;
    char *path;
    uint8_t *data;
} part_t;

static pthread_mutex_t s_mutex = PTHREAD_MUTEX_INITIALIZER;
static part_t s_parts[PARTITION_MAX];
static size_t s_count;
static void *s_maps[MAP_MAX];

static part_t *lookup(const esp_partition_t *partition) {
    for (size_t i = 0; i < s_count; i++) {
        if (&s_parts[i].info == partition) {
            return &s_parts[i];
        }
    }
    return NULL;
}

static esp_err_t save(part_t *p) {
    FILE *f = fopen(p->path, "wb");
    if (!f) {
        fprintf(stderr, "esp_partition: cannot write %s\n", p->path);
        return ESP_FAIL;
    }
    size_t n = fwrite(p->data, 1, p->info.size, f);
    fclose(f);
    return n == p->info.size ? ESP_OK : ESP_FAIL;
}

esp_err_t esp_partition_sim_add(const char *label, size_t size, const char *path, const char *image) {
    if (!label || !path || strlen(label) >= sizeof(s_parts[0].info.label) || size == 0 || size % SECTOR) {
        return ESP_ERR_INVALID_ARG;
    }
    pthread_mutex_lock(&s_mutex);
    for (size_t i = 0; i < s_count; i++) {
        if (strcmp(s_parts[i].info.label, label) == 0) {
            pthread_mutex_unlock(&s_mutex);
            return ESP_OK;
        }
    }
    if (s_count == PARTITION_MAX) {
        pthread_mutex_unlock(&s_mutex);
        return ESP_ERR_NO_MEM;
    }
    part_t *p = &s_parts[s_count];
    p->data = malloc(size);
    p->path = strdup(path);
    if (!p->data || !p->path) {
        free(p->data);
        free(p->path);
        pthread_mutex_unlock(&s_mutex);
        return ESP_ERR_NO_MEM;
    }
    memset(p->data, 0xff, size);
    esp_err_t err = ESP_OK;
    FILE *f = fopen(path, "rb");
    if (f) {
        fread(p->data, 1, size, f);
        fclose(f);
    } else {
        FILE *src = image ? fopen(image, "rb") : NULL;
        if (image && !src) {
            fprintf(stderr, "esp_partition: cannot read %s\n", image);
        }
        if (src) {
            fread(p->data, 1, size, src);
            fclose(src);
        }
    }
    p->info.type = ESP_PARTITION_TYPE_DATA;
    p->info.subtype = ESP_PARTITION_SUBTYPE_DATA_UNDEFINED;
    p->info.size = size;
    p->info.erase_size = SECTOR;
    strcpy(p->info.label, label);
    s_count++;
    if (!f) {
        err = save(p);
    }
    pthread_mutex_unlock(&s_mutex);
    return err;
}

const esp_partition_t *esp_partition_find_first(esp_partition_type_t type, esp_partition_subtype_t subtype,
                                                const char *label) {
    (void)type;
    (void)subtype;
    const esp_partition_t *found = NULL;
    pthread_mutex_lock(&s_mutex);
    for (size_t i = 0; i < s_count && !found; i++) {
        if (!label || strcmp(s_parts[i].info.label, label) == 0) {
            found = &s_parts[i].info;
        }
    }
    pthread_mutex_unlock(&s_mutex);
    return found;
}

static part_t *checked(const esp_partition_t *partition, size_t offset, size_t size) {
    part_t *p = lookup(partition);
    if (!p || offset > p->info.size || size > p->info.size - offset) {
        return NULL;
    }
    return p;
}

esp_err_t esp_partition_read(const esp_partition_t *partition, size_t src_offset, void *dst, size_t size) {
    pthread_mutex_lock(&s_mutex);
    part_t *p = checked(partition, src_offset, size);
    if (p) {
        memcpy(dst, p->data + src_offset, size);
    }
    pthread_mutex_unlock(&s_mutex);
    return p ? ESP_OK : ESP_ERR_INVALID_SIZE;
}

esp_err_t esp_partition_write(const esp_partition_t *partition, size_t dst_offset, const void *src, size_t size) {
    pthread_mutex_lock(&s_mutex);
    part_t *p = checked(partition, dst_offset, size);
    esp_err_t err = ESP_ERR_INVALID_SIZE;
    if (p) {
        const uint8_t *in = src;
        for (size_t i = 0; i < size; i++) {
            p->data[dst_offset + i] &= in[i];
        }
        err = save(p);
    }
    pthread_mutex_unlock(&s_mutex);
    return err;
}

esp_err_t esp_partition_erase_range(const esp_partition_t *partition, size_t offset, size_t size) {
    if (offset % SECTOR || size % SECTOR) {
        return ESP_ERR_INVALID_SIZE;
    }
    pthread_mutex_lock(&s_mutex);
    part_t *p = checked(partition, offset, size);
    esp_err_t err = ESP_ERR_INVALID_SIZE;
    if (p) {
        memset(p->data + offset, 0xff, size);
        err = save(p);
    }
    pthread_mutex_unlock(&s_mutex);
    return err;
}

esp_err_t esp_partition_mmap(const esp_partition_t *partition, size_t offset, size_t size,
                             esp_partition_mmap_flag_t flags, const void **out_ptr,
                             esp_partition_mmap_handle_t *out_handle) {
    (void)flags;
    pthread_mutex_lock(&s_mutex);
    part_t *p = checked(partition, offset, size);
    esp_err_t err = ESP_ERR_INVALID_SIZE;
    if (p) {
        err = ESP_ERR_NO_MEM;
        for (size_t i = 0; i < MAP_MAX; i++) {
            if (!s_maps[i]) {
                void *copy = malloc(size ? size : 1);
                if (copy) {
                    memcpy(copy, p->data + offset, size);
                    s_maps[i] = copy;
                    *out_ptr = copy;
                    *out_handle = (esp_partition_mmap_handle_t)(i + 1);
                    err = ESP_OK;
                }
                break;
            }
        }
    }
    pthread_mutex_unlock(&s_mutex);
    return err;
}

void esp_partition_munmap(esp_partition_mmap_handle_t handle) {
    pthread_mutex_lock(&s_mutex);
    if (handle >= 1 && handle <= MAP_MAX) {
        free(s_maps[handle - 1]);
        s_maps[handle - 1] = NULL;
    }
    pthread_mutex_unlock(&s_mutex);
}
