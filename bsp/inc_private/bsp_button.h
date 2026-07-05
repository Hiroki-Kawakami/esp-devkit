/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Internal button-driver interface. Same struct-inheritance vtable as
 * bsp_display / bsp_touch / bsp_led / bsp_rtc: a driver embeds bsp_button_t
 * as its first member and returns &state->base from its *_create(). The
 * board's bsp_init calls bsp_button_set_active() once; the common layer
 * (src/bsp_button.c) then registers a tick source, runs debounce + the click/
 * double-click/long-press state machine, and dispatches per-event callbacks.
 */

#pragma once
#include "bsp_types.h"

typedef struct bsp_button bsp_button_t;

struct bsp_button {
    uint8_t   count;
    /* Provider wakes the input task on an edge -> the common layer may idle
     * between presses instead of polling continuously. */
    bool      has_int;
    /* Fill up to `max` slots with the current pressed state per button. */
    esp_err_t (*sample)(bsp_button_t *self, bool *pressed, uint8_t max);
    esp_err_t (*deinit)(bsp_button_t *self);
};

void bsp_button_set_active(bsp_button_t *button);
