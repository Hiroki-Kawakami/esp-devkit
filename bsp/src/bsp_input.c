/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "bsp_input.h"

#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "freertos/task.h"

#define BSP_INPUT_MAX_SOURCES     4
#define DEFAULT_POLL_INTERVAL_MS  10
#define DEFAULT_TASK_STACK        3072

static bsp_input_source_t s_sources[BSP_INPUT_MAX_SOURCES];
static uint8_t            s_source_count;

static SemaphoreHandle_t  s_wake_sem;
static TaskHandle_t       s_task;
static bool               s_task_running;
static TickType_t         s_poll_interval;

static inline void ensure_sem(void) {
    if (!s_wake_sem) s_wake_sem = xSemaphoreCreateBinary();
}

esp_err_t bsp_input_add_source(const bsp_input_source_t *source) {
    if (!source || !source->tick) return ESP_ERR_INVALID_ARG;
    if (s_source_count >= BSP_INPUT_MAX_SOURCES) return ESP_ERR_NO_MEM;
    s_sources[s_source_count++] = *source;
    bsp_input_notify();
    return ESP_OK;
}

void bsp_input_notify(void) {
    ensure_sem();
    if (s_wake_sem) xSemaphoreGive(s_wake_sem);
}

void bsp_input_notify_from_isr(BaseType_t *hp) {
    if (s_wake_sem) xSemaphoreGiveFromISR(s_wake_sem, hp);
}

static void input_task(void *arg) {
    (void)arg;
    for (;;) {
        bool any_active = false;
        for (uint8_t i = 0; i < s_source_count; i++) {
            if (s_sources[i].tick(s_sources[i].ctx)) any_active = true;
        }
        xSemaphoreTake(s_wake_sem, any_active ? s_poll_interval : portMAX_DELAY);
    }
}

esp_err_t bsp_input_start(uint8_t priority, int8_t affinity,
                          uint32_t poll_interval_ms, uint32_t task_stack) {
    if (priority == 0)  return ESP_OK;
    if (s_task_running) return ESP_OK;
    ensure_sem();
    if (!s_wake_sem) return ESP_ERR_NO_MEM;

    s_poll_interval = pdMS_TO_TICKS(poll_interval_ms ? poll_interval_ms : DEFAULT_POLL_INTERVAL_MS);
    BaseType_t core = affinity < 0 ? tskNO_AFFINITY : (BaseType_t)affinity;
    uint32_t stack = task_stack ? task_stack : DEFAULT_TASK_STACK;
    if (xTaskCreatePinnedToCore(input_task, "bsp_input", stack, NULL,
                                priority, &s_task, core) != pdPASS) {
        return ESP_FAIL;
    }
    s_task_running = true;
    return ESP_OK;
}

bool bsp_input_running(void) { return s_task_running; }
