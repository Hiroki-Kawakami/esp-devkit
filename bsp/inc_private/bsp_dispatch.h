/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Shared lightweight-task multiplexer. Any subsystem that needs periodic or
 * deferred work (touch chip, button GPIO sampler, audio route tracking, ...)
 * registers a static bsp_dispatch_source_t; each tick returns the number of ms
 * until it wants to run again (0 = immediate re-run, BSP_DISPATCH_IDLE = sleep
 * until notified). bsp_dispatch_notify(_from_isr) forces a source's tick on the
 * next iteration -- the ISR-wakeup path. The task auto-starts on the first
 * bsp_dispatch_add_source call; bsp_dispatch_configure (called from bsp_init
 * before any device registers) only tunes its priority/affinity.
 */

#pragma once
#include "bsp_types.h"
#include "freertos/FreeRTOS.h"
#include "freertos/portmacro.h"

#define BSP_DISPATCH_IDLE  UINT32_MAX   /* sleep until notified */

typedef struct bsp_dispatch_source {
    uint32_t (*tick)(void *ctx);   /* returns ms until next tick; 0 = immediate re-run; BSP_DISPATCH_IDLE = sleep until notify */
    void *ctx;
    /* dispatch-internal, zero-init; caller owns the struct (static storage) */
    struct bsp_dispatch_source *next;
    TickType_t deadline;
    bool waiting;
    volatile bool pending;
} bsp_dispatch_source_t;

/* Static registration -- no unregister. Re-registering the same source is a
 * no-op. Starts the task on the first call. */
esp_err_t bsp_dispatch_add_source(bsp_dispatch_source_t *src);

void bsp_dispatch_notify(bsp_dispatch_source_t *src);
void bsp_dispatch_notify_from_isr(bsp_dispatch_source_t *src, BaseType_t *hp);

/* Priority/affinity for the task the first bsp_dispatch_add_source spawns;
 * only effective before that happens. priority 0 -> default (5); affinity < 0
 * -> no affinity. */
void bsp_dispatch_configure(uint8_t priority, int8_t affinity);

/* Idempotent install of the shared GPIO ISR service, so several INT-driven
 * providers can require it without the double-install error. */
esp_err_t bsp_dispatch_install_gpio_isr(void);

bool bsp_dispatch_running(void);
