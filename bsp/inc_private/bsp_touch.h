/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Internal touch-driver interface. A chip driver embeds bsp_touch_t as its first
 * member and fills the poll/deinit vtable plus the base geometry/INT fields; the
 * common layer (src/bsp_touch.c) owns the reader task, INT ISR, INT->poll->INT
 * state machine, orientation transform, and the cached snapshot behind the public
 * bsp_touch_* API. Chips do not touch task/ISR/orientation code.
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

struct bsp_touch {
    /* Chip vtable ---------------------------------------------------------- */

    /* Read one frame from the chip. Fill up to `max` raw (chip-space) points into
     * `out`, set *count. Set *fresh = true if a new frame was actually read
     * (count 0 with fresh = true is a release). Set *keep_polling = true to ask
     * the common layer to keep ticking regardless of touch state (e.g. GT911
     * HotKnot session). */
    esp_err_t (*poll)(bsp_touch_t *self,
                      bsp_touch_raw_point_t *out, uint8_t max,
                      uint8_t *count, bool *fresh, bool *keep_polling);
    esp_err_t (*deinit)(bsp_touch_t *self);

    /* Chip fills at create-time; common layer reads ------------------------- */
    uint16_t width, height;    /* display space */
    uint8_t  max_points;       /* 0 -> library max */
    bool     swap_xy, mirror_x, mirror_y;
    int      int_io;           /* INT pin (gpio_num_t compatible; < 0 -> none) */
    uint8_t  settle_count;     /* consecutive no-touch polls before idling; 0 -> default */

    /* Common layer owns. Set by the INT ISR the common layer attaches. */
    volatile bool int_pending;
};

/* Register the active touch panel with the common layer. Attaches the shared INT
 * ISR to touch->int_io if it is >= 0. */
void bsp_touch_set_active(bsp_touch_t *touch);

/* Start the shared reader task on the active touch. priority == 0 -> no task
 * (bsp_touch_read sync-polls the chip on demand). Idempotent: subsequent starts
 * return ESP_ERR_INVALID_STATE without disturbing the running task. */
esp_err_t bsp_touch_start_reader(uint8_t priority, int8_t affinity,
                                 uint32_t poll_interval_ms, uint32_t task_stack);

/* True once bsp_touch_start_reader has spawned the reader task. HotKnot uses
 * this to gate its session state machine. */
bool bsp_touch_reader_running(void);

/* Provider upcall: dispatch display-space contacts to bsp_touch_set_event_cb and
 * cache them for bsp_touch_read. Called by the reader task, the simulator, and
 * chip drivers on a sync poll. count 0 = all released. */
void bsp_touch_emit_event(const bsp_touch_point_t *points, int count);
