/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Internal button-driver interface. Providers are appended (not replaced) into a
 * flat, add-order button id space; the public bsp_button_* API in bsp.h indexes
 * that space. Two distinct provider types, on two layers:
 *
 *   bsp_button_t + bsp_button_add()  -- the lower layer, tick-driven. The provider
 *       owns a dispatch source (one per provider) via its tick(); it detects its
 *       own events and reports them with bsp_button_emit(). No state machine, no
 *       notion of "level" here.
 *
 *   bsp_button_raw_t + bsp_button_add_raw()  -- the level layer. The provider only
 *       fills sample() with live pressed states; the common layer wraps it with
 *       the shared debounce + click/double-click/long-press state machine (a
 *       synthesized bsp_button_t that emits) and registers that via
 *       bsp_button_add(). A convenience layer on top; its own struct so it carries
 *       none of the lower layer's fields.
 *
 * A driver embeds the matching struct as its first member and returns
 * &state->base from its *_create(); the board adds it once.
 */

#pragma once
#include "bsp_types.h"
#include "bsp_dispatch.h"
#include "freertos/FreeRTOS.h"

typedef struct bsp_button bsp_button_t;
typedef struct bsp_button_raw bsp_button_raw_t;

typedef enum {
    BSP_BUTTON_EVENT_DOWN,
    BSP_BUTTON_EVENT_UP,
    BSP_BUTTON_EVENT_CLICK,
    BSP_BUTTON_EVENT_DOUBLE_CLICK,
    BSP_BUTTON_EVENT_LONG_PRESS,
} bsp_button_event_t;

/* Snapshot of what the app has registered for one button, pushed to a provider's
 * on_config so it can mirror the intent in hardware (e.g. a long-press threshold)
 * once at registration instead of polling. */
typedef struct {
    bool     down, up, click, double_click, long_press;  /* which callbacks exist */
    uint16_t double_ms;
    uint16_t long_ms;
} bsp_button_reg_t;

/* Lower layer: tick-driven provider that emits its own events. */
struct bsp_button {
    uint8_t   count;
    /* Per-tick hook: detect + emit; returns ms until the next tick (0 = immediate,
     * BSP_DISPATCH_IDLE = sleep until notify, N = poll in N ms). */
    uint32_t (*tick)(bsp_button_t *self);
    /* Optional. A callback registration for local button `local_id` changed; runs
     * on the registering thread. NULL when the provider needs no config. */
    void     (*on_config)(bsp_button_t *self, uint8_t local_id, const bsp_button_reg_t *reg);
    esp_err_t (*deinit)(bsp_button_t *self);

    /* Filled by the framework on add; providers must not set these. */
    uint8_t                base_id;
    bsp_dispatch_source_t *source;
};

/* Level layer: reports live pressed states; the framework runs the shared state
 * machine on top. */
struct bsp_button_raw {
    uint8_t   count;
    /* Fill up to `max` slots with the current pressed state per button. */
    esp_err_t (*sample)(bsp_button_raw_t *self, bool *pressed, uint8_t max);
    /* Provider wakes its source on an edge, so the wrapper idles between presses
     * instead of polling. */
    bool      has_int;
    /* Poll cadence when has_int is false; 0 -> default. */
    uint16_t  poll_ms;
    esp_err_t (*deinit)(bsp_button_raw_t *self);

    /* Filled by the framework on add (the wrapping provider's source, for the
     * edge ISR to wake); providers must not set it. */
    bsp_dispatch_source_t *source;
};

/* Append a tick-driven provider (lower layer). */
esp_err_t bsp_button_add(bsp_button_t *provider);

/* Append a level provider, wrapping it with the shared state machine. */
esp_err_t bsp_button_add_raw(bsp_button_raw_t *provider);

/* Route an event straight to button `id`'s registered callback. Called from a
 * provider tick() (on the dispatch task); the state-machine wrapper uses it too. */
void bsp_button_emit(uint8_t id, bsp_button_event_t event);

/* Wake `self`'s dispatch source from the provider's GPIO ISR. */
void bsp_button_notify_from_isr(bsp_button_raw_t *self, BaseType_t *hp);
