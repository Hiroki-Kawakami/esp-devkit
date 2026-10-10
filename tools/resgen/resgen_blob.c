/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "resgen_blob.h"
#include <stdlib.h>
#include <string.h>
#include "esp_log.h"
#ifdef ESP_PLATFORM
#include "esp_partition.h"
#else
#include <stdio.h>
#endif

static const char *TAG = "resgen";

const uint8_t *resgen_blob_map(const char *partition, const char *path,
                               const uint8_t *header, size_t header_size, size_t size) {
#ifdef ESP_PLATFORM
    (void)path;
    const esp_partition_t *part =
        esp_partition_find_first(ESP_PARTITION_TYPE_DATA, ESP_PARTITION_SUBTYPE_ANY, partition);
    if (!part || part->size < size) {
        ESP_LOGE(TAG, "partition '%s' is missing or smaller than %u bytes", partition, (unsigned)size);
        abort();
    }
    const void *blob = NULL;
    esp_partition_mmap_handle_t handle;
    esp_err_t err = esp_partition_mmap(part, 0, size, ESP_PARTITION_MMAP_DATA, &blob, &handle);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "cannot map partition '%s': %s", partition, esp_err_to_name(err));
        abort();
    }
#else
    (void)partition;
    uint8_t *blob = malloc(size);
    FILE *file = fopen(path, "rb");
    if (!blob || !file || fread(blob, 1, size, file) != size) {
        ESP_LOGE(TAG, "cannot read %s", path);
        abort();
    }
    fclose(file);
#endif
    if (memcmp(blob, header, header_size) != 0) {
        ESP_LOGE(TAG, "partition '%s' does not match this firmware; write it with `idf.py flash`", partition);
        abort();
    }
    return blob;
}
