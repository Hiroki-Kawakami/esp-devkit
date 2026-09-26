/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "logfwd.h"

#include <stdio.h>

#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "freertos/stream_buffer.h"
#include "freertos/task.h"
#include "transport.h"

#define RING_SIZE   4096
#define LINE_LEN    256
#define CHUNK       1024

static StreamBufferHandle_t s_ring;
static SemaphoreHandle_t s_lock;
static vprintf_like_t s_orig;
static TaskHandle_t s_task;
static volatile bool s_enabled;

static int hook(const char *fmt, va_list ap) {
    char line[LINE_LEN];
    va_list copy;
    va_copy(copy, ap);
    int n = vsnprintf(line, sizeof(line), fmt, copy);
    va_end(copy);
    if (n > 0 && xSemaphoreTake(s_lock, pdMS_TO_TICKS(5)) == pdTRUE) {
        xStreamBufferSend(s_ring, line, n < LINE_LEN ? n : LINE_LEN - 1, 0);
        xSemaphoreGive(s_lock);
    }
    return s_orig(fmt, ap);
}

static void drain_task(void *arg) {
    static uint8_t chunk[CHUNK];
    for (;;) {
        if (!s_enabled || !transport_is_open()) {
            ulTaskNotifyTake(pdTRUE, pdMS_TO_TICKS(100));
            if (xStreamBufferSpacesAvailable(s_ring) < CHUNK) {
                xStreamBufferReceive(s_ring, chunk, CHUNK, 0);
            }
            continue;
        }
        size_t n = xStreamBufferReceive(s_ring, chunk, sizeof(chunk), pdMS_TO_TICKS(100));
        if (n) transport_send(HOSTED_IF_LOG, 0, 0, chunk, n, pdMS_TO_TICKS(20));
    }
}

void logfwd_init(void) {
    s_ring = xStreamBufferCreate(RING_SIZE, 1);
    s_lock = xSemaphoreCreateMutex();
    assert(s_ring && s_lock);
    xTaskCreate(drain_task, "logfwd", 2560, NULL, 5, &s_task);
    s_orig = esp_log_set_vprintf(hook);
}

void logfwd_enable(bool enable) {
    s_enabled = enable;
    xTaskNotifyGive(s_task);
}
