/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Model-agnostic touch layer: holds the active bsp_touch provider (registered by
 * the board's bsp_init via bsp_touch_set_active) and implements the public
 * bsp_touch_* API by dispatching through its vtable. The mirror of src/bsp_display.c.
 */

#include "bsp.h"
#include "bsp_touch.h"

static bsp_touch_t *s_touch;
static bsp_touch_event_cb_t s_event_cb;
static void               *s_event_arg;

void bsp_touch_set_active(bsp_touch_t *touch) {
    s_touch = touch;
}

void bsp_touch_set_event_cb(bsp_touch_event_cb_t cb, void *arg) {
    s_event_arg = arg;
    s_event_cb  = cb;
}

void bsp_touch_emit_event(const bsp_touch_point_t *points, int count) {
    bsp_touch_event_cb_t cb = s_event_cb;
    if (cb) cb(points, count, s_event_arg);
}

int bsp_touch_read(bsp_touch_point_t *points, uint8_t max_points) {
    if (!s_touch || !s_touch->read || !points || max_points == 0) return 0;
    return s_touch->read(s_touch, points, max_points);
}

void bsp_touch_wait_interrupt(void) {
    if (s_touch && s_touch->wait_interrupt) s_touch->wait_interrupt(s_touch);
}
