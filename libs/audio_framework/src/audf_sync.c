/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_sync.h"

esp_err_t audf_sync_init(audf_sync_t *sync) {
    sync->lock = xSemaphoreCreateMutex();
    atomic_init(&sync->dirty, false);
    return sync->lock ? ESP_OK : ESP_ERR_NO_MEM;
}

void audf_sync_deinit(audf_sync_t *sync) {
    if (sync->lock) vSemaphoreDelete(sync->lock);
    sync->lock = NULL;
}

void audf_sync_lock(audf_sync_t *sync) {
    xSemaphoreTake(sync->lock, portMAX_DELAY);
}

void audf_sync_unlock(audf_sync_t *sync) {
    xSemaphoreGive(sync->lock);
}

void audf_sync_publish(audf_sync_t *sync) {
    atomic_store(&sync->dirty, true);
    xSemaphoreGive(sync->lock);
}

bool audf_sync_try_adopt(audf_sync_t *sync) {
    if (!atomic_load(&sync->dirty)) return false;
    if (xSemaphoreTake(sync->lock, 0) != pdTRUE) return false;
    atomic_store(&sync->dirty, false);
    return true;
}
