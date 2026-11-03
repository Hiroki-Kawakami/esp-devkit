/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "executor_shim.h"

static portMUX_TYPE s_mux __attribute__((unused)) = portMUX_INITIALIZER_UNLOCKED;

void idf_executor_notify(TaskHandle_t task) {
#ifdef ESP_PLATFORM
    if (xPortInIsrContext()) {
        BaseType_t woken = pdFALSE;
        vTaskNotifyGiveFromISR(task, &woken);
        portYIELD_FROM_ISR(woken);
        return;
    }
#endif
    xTaskNotifyGive(task);
}

void idf_executor_critical_enter(void) {
    portENTER_CRITICAL_SAFE(&s_mux);
}

void idf_executor_critical_exit(void) {
    portEXIT_CRITICAL_SAFE(&s_mux);
}
