/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "executor_shim.h"

#ifdef ESP_PLATFORM
#include "hal/cache_ll.h"
#endif

static portMUX_TYPE s_mux __attribute__((unused)) = portMUX_INITIALIZER_UNLOCKED;
static portMUX_TYPE s_sync_mux __attribute__((unused)) = portMUX_INITIALIZER_UNLOCKED;

void idf_task_notify(TaskHandle_t task, UBaseType_t index, uint32_t bits) {
#ifdef ESP_PLATFORM
    if (xPortInIsrContext()) {
        BaseType_t woken = pdFALSE;
        xTaskGenericNotifyFromISR(task, index, bits, eSetBits, NULL, &woken);
        portYIELD_FROM_ISR(woken);
        return;
    }
#endif
    xTaskGenericNotify(task, index, bits, eSetBits, NULL);
}

void idf_executor_critical_enter(void) {
    portENTER_CRITICAL_SAFE(&s_mux);
}

void idf_executor_critical_exit(void) {
    portEXIT_CRITICAL_SAFE(&s_mux);
}

void idf_sync_critical_enter(void) {
    portENTER_CRITICAL(&s_sync_mux);
}

void idf_sync_critical_exit(void) {
    portEXIT_CRITICAL(&s_sync_mux);
}

void *idf_cache_uncached(void *cached) {
#ifdef CACHE_LL_L2MEM_NON_CACHE_ADDR
    return (void *)CACHE_LL_L2MEM_NON_CACHE_ADDR(cached);
#else
    return cached;
#endif
}
