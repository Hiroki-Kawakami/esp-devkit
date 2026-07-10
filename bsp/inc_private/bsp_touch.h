/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Internal touch-driver interface. A chip driver embeds bsp_touch_t as its first
 * member and fills the poll/deinit vtable plus the base geometry/INT fields; the
 * common layer (src/bsp_touch.c) owns the INT ISR, INT->poll->INT state machine,
 * orientation transform, and the cached snapshot behind the public bsp_touch_*
 * API, and registers a bsp_dispatch source to drive polling. Chips do not touch
 * dispatch/ISR/orientation code.
 */

#pragma once
#include "bsp_types.h"

/* Raw chip-space touch point emitted by bsp_touch_t::poll. The common layer maps
 * it into display space using the swap_xy / mirror_* fields on the base struct. */
typedef struct {
    uint16_t x;
    uint16_t y;
    uint8_t  id;
} bsp_touch_raw_point_t;

typedef struct bsp_touch bsp_touch_t;

/* int_io sentinel: the chip has an INT, but it is delivered by an external caller
 * of bsp_touch_notify() (e.g. aggregated on an I/O expander) rather than a native
 * GPIO the common layer should attach an ISR to. Enables the idle capability
 * without owning a pin. (GPIO_NUM_NC == -1 -> no INT; a value >= 0 -> native GPIO.) */
#define BSP_TOUCH_INT_EXTERNAL  (-2)

struct bsp_touch {
    /* Return the current best-known touch state. Chips with a data-ready
     * handshake cache the last authoritative frame and return it on polls
     * where nothing new arrived -- callers only see counts, never "unknown",
     * and press/release edges are derived from consecutive polls. Set
     * *keep_polling to keep the task ticking irrespective of touch state
     * (GT911 HotKnot uses this to run its session step). */
    esp_err_t (*poll)(bsp_touch_t *self,
                      bsp_touch_raw_point_t *out, uint8_t max,
                      uint8_t *count, bool *keep_polling);
    esp_err_t (*deinit)(bsp_touch_t *self);

    /* Chip fills at create-time; common layer reads ------------------------- */
    uint16_t width, height;    /* display space */
    uint8_t  max_points;       /* 0 -> library max */
    bool     swap_xy, mirror_x, mirror_y;
    int      int_io;           /* INT pin (gpio_num_t compatible; < 0 -> none) */
    uint8_t  settle_count;     /* consecutive no-touch polls before idling; 0 -> default */
    uint16_t poll_interval_ms; /* dispatch re-tick interval while active; 0 -> default (10ms) */

    /* Common layer owns. Set by the INT ISR the common layer attaches. */
    volatile bool int_pending;
};

/* Register the active touch panel with the common layer: attaches the shared
 * INT ISR to touch->int_io if it is >= 0 and registers a bsp_dispatch source
 * that drives polling -- no separate start call needed. */
void bsp_touch_set_active(bsp_touch_t *touch);

/* Simulate an INT edge: mark int_pending and wake the dispatch source. The
 * device INT ISR does this; the simulator's SDL touch provider calls it from
 * the input path so host + device share the same dispatch-driven flow. */
void bsp_touch_notify(void);

/* Provider upcall: dispatch display-space contacts to bsp_touch_set_event_cb and
 * cache them for bsp_touch_read. Called by the dispatch source, the simulator,
 * and chip drivers on a sync poll. count 0 = all released. */
void bsp_touch_emit_event(const bsp_touch_point_t *points, int count);
