/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "bsp_dispatch.h"

#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "freertos/task.h"
#ifdef ESP_PLATFORM
#include "driver/gpio.h"
#endif

#define DEFAULT_PRIORITY  5
#define TASK_STACK        4096

static bsp_dispatch_source_t *s_sources;
static portMUX_TYPE           s_list_mux = portMUX_INITIALIZER_UNLOCKED;

static SemaphoreHandle_t s_wake_sem;
static TaskHandle_t      s_task;
static uint8_t           s_priority;
static int8_t            s_affinity = -1;

static inline void ensure_sem(void) {
    if (!s_wake_sem) s_wake_sem = xSemaphoreCreateBinary();
}

void bsp_dispatch_configure(uint8_t priority, int8_t affinity) {
    s_priority = priority;
    s_affinity = affinity;
}

static void dispatch_task(void *arg) {
    (void)arg;
    for (;;) {
        TickType_t now = xTaskGetTickCount();
        for (bsp_dispatch_source_t *src = s_sources; src; src = src->next) {
            if (src->pending || (!src->waiting && (int32_t)(src->deadline - now) <= 0)) {
                src->pending = false;   /* before tick, so an ISR during tick re-arms */
                uint32_t ms = src->tick(src->ctx);
                src->waiting = (ms == BSP_DISPATCH_IDLE);
                if (!src->waiting) {
                    TickType_t d = pdMS_TO_TICKS(ms);
                    if (ms > 0 && d == 0) d = 1;   /* sub-tick interval must not busy-spin */
                    src->deadline = now + d;
                }
            }
        }

        now = xTaskGetTickCount();   /* re-read: ticks may have taken time (I2C) */
        TickType_t wait = portMAX_DELAY;
        for (bsp_dispatch_source_t *src = s_sources; src; src = src->next) {
            if (src->waiting) continue;
            int32_t remain = (int32_t)(src->deadline - now);
            TickType_t w = remain <= 0 ? 0 : (TickType_t)remain;
            if (w < wait) wait = w;
        }
        xSemaphoreTake(s_wake_sem, wait);
    }
}

esp_err_t bsp_dispatch_add_source(bsp_dispatch_source_t *src) {
    if (!src || !src->tick) return ESP_ERR_INVALID_ARG;

    bool already = false;
    portENTER_CRITICAL(&s_list_mux);
    for (bsp_dispatch_source_t *s = s_sources; s; s = s->next) {
        if (s == src) { already = true; break; }
    }
    if (!already) {
        src->pending = true;
        src->next    = s_sources;
        s_sources    = src;
    }
    portEXIT_CRITICAL(&s_list_mux);
    if (already) return ESP_OK;

    ensure_sem();
    if (!s_wake_sem) return ESP_ERR_NO_MEM;

    if (!s_task) {
        uint8_t priority = s_priority ? s_priority : DEFAULT_PRIORITY;
        BaseType_t core = s_affinity < 0 ? tskNO_AFFINITY : (BaseType_t)s_affinity;
        if (xTaskCreatePinnedToCore(dispatch_task, "bsp_dispatch", TASK_STACK, NULL,
                                    priority, &s_task, core) != pdPASS) {
            return ESP_FAIL;
        }
    }
    xSemaphoreGive(s_wake_sem);
    return ESP_OK;
}

void bsp_dispatch_notify(bsp_dispatch_source_t *src) {
    src->pending = true;
    if (s_wake_sem) xSemaphoreGive(s_wake_sem);
}

void bsp_dispatch_notify_from_isr(bsp_dispatch_source_t *src, BaseType_t *hp) {
    src->pending = true;
    if (s_wake_sem) xSemaphoreGiveFromISR(s_wake_sem, hp);
}

esp_err_t bsp_dispatch_install_gpio_isr(void) {
#ifdef ESP_PLATFORM
    static bool installed;
    if (installed) return ESP_OK;
    esp_err_t err = gpio_install_isr_service(0);
    if (err != ESP_OK && err != ESP_ERR_INVALID_STATE) return err;
    installed = true;
#endif
    return ESP_OK;
}

bool bsp_dispatch_running(void) { return s_task != NULL; }
