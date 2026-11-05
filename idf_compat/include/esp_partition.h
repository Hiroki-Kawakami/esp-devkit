#pragma once
// Host counterpart of ESP-IDF <esp_partition.h>: data partitions backed by
// files, enough to find, read, write, erase and map one by label.
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    ESP_PARTITION_MMAP_DATA = 0,
    ESP_PARTITION_MMAP_INST = 1 << 0,
    ESP_PARTITION_MMAP_BLOCKS_WRITE = 1 << 1,
} esp_partition_mmap_flag_t;

typedef uint32_t esp_partition_mmap_handle_t;

typedef enum {
    ESP_PARTITION_TYPE_APP = 0x00,
    ESP_PARTITION_TYPE_DATA = 0x01,
    ESP_PARTITION_TYPE_ANY = 0xff,
} esp_partition_type_t;

typedef enum {
    ESP_PARTITION_SUBTYPE_DATA_UNDEFINED = 0x06,
    ESP_PARTITION_SUBTYPE_ANY = 0xff,
} esp_partition_subtype_t;

typedef struct esp_flash_t esp_flash_t;

typedef struct {
    esp_flash_t *flash_chip;
    esp_partition_type_t type;
    esp_partition_subtype_t subtype;
    uint32_t address;
    uint32_t size;
    uint32_t erase_size;
    char label[17];
    bool encrypted;
    bool readonly;
} esp_partition_t;

/* Type and subtype are not checked; only partitions added with
 * esp_partition_sim_add() exist. */
const esp_partition_t *esp_partition_find_first(esp_partition_type_t type, esp_partition_subtype_t subtype,
                                                const char *label);
esp_err_t esp_partition_read(const esp_partition_t *partition, size_t src_offset, void *dst, size_t size);
/* As on NOR flash, writing only clears bits. */
esp_err_t esp_partition_write(const esp_partition_t *partition, size_t dst_offset, const void *src, size_t size);
esp_err_t esp_partition_erase_range(const esp_partition_t *partition, size_t offset, size_t size);
/* A copy taken at the call; writes after it do not show through. */
esp_err_t esp_partition_mmap(const esp_partition_t *partition, size_t offset, size_t size,
                             esp_partition_mmap_flag_t flags, const void **out_ptr,
                             esp_partition_mmap_handle_t *out_handle);
void esp_partition_munmap(esp_partition_mmap_handle_t handle);

/* Simulator-only extension: a data partition of `size` bytes kept in the file
 * `path`. When the file does not exist it is created erased, with `image` (may
 * be NULL) written at its start, as `idf.py flash` would. Has no on-device
 * counterpart. */
esp_err_t esp_partition_sim_add(const char *label, size_t size, const char *path, const char *image);

#ifdef __cplusplus
}
#endif
