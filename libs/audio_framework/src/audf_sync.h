/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdatomic.h>
#include <stdbool.h>

#include "esp_err.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"

/* Setters write pending parameters under the lock; process adopts them at a
 * block boundary only when the lock is free, so it never waits on a setter. */
typedef struct {
    SemaphoreHandle_t lock;
    atomic_bool       dirty;
} audf_sync_t;

esp_err_t audf_sync_init(audf_sync_t *sync);
void      audf_sync_deinit(audf_sync_t *sync);
void      audf_sync_lock(audf_sync_t *sync);
void      audf_sync_unlock(audf_sync_t *sync);
void      audf_sync_publish(audf_sync_t *sync);
/* True with the lock held when there is something to adopt. */
bool      audf_sync_try_adopt(audf_sync_t *sync);
