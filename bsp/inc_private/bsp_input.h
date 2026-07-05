/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Shared input reader-task infrastructure. Any subsystem that needs periodic
 * polling (touch chip, button GPIO sampler, ...) registers a tick callback as
 * a source; the task calls every registered tick per iteration and sleeps in
 * between. Each tick returns whether it still needs polling: while any source
 * is active the task wakes on poll_interval, otherwise it blocks on the shared
 * wake sem until an ISR (or bsp_input_notify) fires.
 */

#pragma once
#include "bsp_types.h"
#include "freertos/FreeRTOS.h"
#include "freertos/portmacro.h"

typedef struct {
    bool (*tick)(void *ctx);
    void *ctx;
} bsp_input_source_t;

/* Static registration -- no unregister. Fires a notify so a source that starts
 * pre-active gets its first tick promptly. */
esp_err_t bsp_input_add_source(const bsp_input_source_t *source);

void bsp_input_notify(void);
void bsp_input_notify_from_isr(BaseType_t *hp);

/* Idempotent install of the shared GPIO ISR service, so several INT-driven
 * providers can require it without the double-install error. */
esp_err_t bsp_input_install_gpio_isr(void);

/* Idempotent. priority == 0 -> no task is spawned (sources are dormant but
 * registration still works -- useful when only the sync-poll paths are wanted). */
esp_err_t bsp_input_start(uint8_t priority, int8_t affinity,
                          uint32_t poll_interval_ms, uint32_t task_stack);
bool bsp_input_running(void);
