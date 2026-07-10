/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Common touch layer: dispatches the public bsp_touch_* API through the active
 * chip vtable and owns the INT ISR, orientation transform, and cached snapshot.
 * bsp_touch_set_active registers a bsp_dispatch source that drives polling --
 * boards no longer start anything themselves.
 *
 * IDLE is a capability that needs an INT line: with one, the tick returns
 * BSP_DISPATCH_IDLE after settle_count consecutive no-finger polls and the ISR
 * notifies the source to wake it again. Boards that leave INT NC (e.g.
 * s31_korvo) never idle -- the tick keeps requesting poll_interval_ms.
 */

#include "bsp.h"
#include "bsp_touch.h"
#include "bsp_dispatch.h"

#include <string.h>

#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "freertos/task.h"

#ifdef ESP_PLATFORM
#include "driver/gpio.h"
#include "esp_log.h"
static const char *TAG = "bsp_touch";
#endif

#define BSP_TOUCH_MAX_POINTS       10
#define DEFAULT_SETTLE_COUNT       5   /* ~50ms at 10ms poll -> settle before idling */

static bsp_touch_t         *s_touch;
static bsp_touch_event_cb_t s_event_cb;
static void                *s_event_arg;

static SemaphoreHandle_t s_snapshot_lock;
static bsp_touch_point_t s_snapshot[BSP_TOUCH_MAX_POINTS];
static uint8_t           s_snapshot_count;

/* Kept alongside the dispatch source so a wait_interrupt caller can be
 * signalled independently of the dispatch task's own wake. */
static SemaphoreHandle_t s_wait_sem;

static uint8_t s_no_touch_count;
static bool    s_was_down;

static uint32_t touch_tick(void *ctx);
static bsp_dispatch_source_t s_touch_source = { .tick = touch_tick };

#ifdef ESP_PLATFORM
static bool s_int_isr_attached;
#endif

/* ------------------------------------------------------------------------- */

void bsp_touch_set_event_cb(bsp_touch_event_cb_t cb, void *arg) {
    s_event_arg = arg;
    s_event_cb  = cb;
}

static inline void ensure_snapshot_lock(void) {
    if (!s_snapshot_lock) s_snapshot_lock = xSemaphoreCreateMutex();
}
static inline void snapshot_lock(void)   { if (s_snapshot_lock) xSemaphoreTake(s_snapshot_lock, portMAX_DELAY); }
static inline void snapshot_unlock(void) { if (s_snapshot_lock) xSemaphoreGive(s_snapshot_lock); }

void bsp_touch_emit_event(const bsp_touch_point_t *points, int count) {
    if (count < 0) count = 0;
    if (count > BSP_TOUCH_MAX_POINTS) count = BSP_TOUCH_MAX_POINTS;

    ensure_snapshot_lock();
    snapshot_lock();
    if (points && count > 0) memcpy(s_snapshot, points, (size_t)count * sizeof(*points));
    s_snapshot_count = (uint8_t)count;
    snapshot_unlock();

    bsp_touch_event_cb_t cb = s_event_cb;
    if (cb) cb(points, count, s_event_arg);
}

void bsp_touch_notify(void) {
    if (s_touch) s_touch->int_pending = true;
    bsp_dispatch_notify(&s_touch_source);
    if (s_wait_sem) xSemaphoreGive(s_wait_sem);
}

static void apply_orientation(const bsp_touch_t *t,
                              const bsp_touch_raw_point_t *in,
                              bsp_touch_point_t *out, int n) {
    for (int i = 0; i < n; i++) {
        int x = in[i].x, y = in[i].y;
        if (t->swap_xy)                       { int tmp = x; x = y; y = tmp; }
        if (t->mirror_x && t->width)          x = (int)t->width  - 1 - x;
        if (t->mirror_y && t->height)         y = (int)t->height - 1 - y;
        out[i].x  = x;
        out[i].y  = y;
        out[i].id = in[i].id;
    }
}

int bsp_touch_read(bsp_touch_point_t *points, uint8_t max_points) {
    if (!s_touch || !points || max_points == 0) return 0;

    snapshot_lock();
    uint8_t n = s_snapshot_count;
    if (n > max_points) n = max_points;
    if (n) memcpy(points, s_snapshot, (size_t)n * sizeof(*points));
    snapshot_unlock();
    return n;
}

void bsp_touch_wait_interrupt(void) {
    if (!s_wait_sem) {
        vTaskDelay(pdMS_TO_TICKS(10));
        return;
    }
    xSemaphoreTake(s_wait_sem, portMAX_DELAY);
}

/* ------------------------------------------------------------------------- */

#ifdef ESP_PLATFORM

static void IRAM_ATTR touch_int_isr(void *arg) {
    bsp_touch_t *t = arg;
    t->int_pending = true;
    BaseType_t hp = pdFALSE;
    bsp_dispatch_notify_from_isr(&s_touch_source, &hp);
    if (s_wait_sem) xSemaphoreGiveFromISR(s_wait_sem, &hp);
    if (hp) portYIELD_FROM_ISR();
}

static void attach_isr(bsp_touch_t *t) {
    if (!t || t->int_io < 0 || s_int_isr_attached) return;
    if (bsp_dispatch_install_gpio_isr() != ESP_OK) return;
    gpio_set_intr_type((gpio_num_t)t->int_io, GPIO_INTR_NEGEDGE);
    if (gpio_isr_handler_add((gpio_num_t)t->int_io, touch_int_isr, t) != ESP_OK) return;
    gpio_intr_enable((gpio_num_t)t->int_io);
    s_int_isr_attached = true;
}
#endif  /* ESP_PLATFORM */

static uint32_t touch_tick(void *ctx) {
    (void)ctx;
    bsp_touch_t *t = s_touch;
    if (!t || !t->poll) return BSP_DISPATCH_IDLE;

    const uint8_t settle  = t->settle_count ? t->settle_count : DEFAULT_SETTLE_COUNT;
    const bool    has_int = (t->int_io >= 0) || (t->int_io == BSP_TOUCH_INT_EXTERNAL);
    const uint32_t interval = t->poll_interval_ms ? t->poll_interval_ms : 10;

    if (has_int && !t->int_pending && s_no_touch_count >= settle && !s_was_down)
        return BSP_DISPATCH_IDLE;
    t->int_pending = false;

    bsp_touch_raw_point_t raw[BSP_TOUCH_MAX_POINTS];
    uint8_t max = t->max_points ? t->max_points : BSP_TOUCH_MAX_POINTS;
    if (max > BSP_TOUCH_MAX_POINTS) max = BSP_TOUCH_MAX_POINTS;
    uint8_t n = 0;
    bool keep_polling = false;

    if (t->poll(t, raw, max, &n, &keep_polling) != ESP_OK) return interval;

    bsp_touch_point_t pts[BSP_TOUCH_MAX_POINTS];
    apply_orientation(t, raw, pts, n);
    if (n > 0) {
        bsp_touch_emit_event(pts, n);
        s_was_down = true;
        s_no_touch_count = 0;
    } else {
        if (s_was_down) bsp_touch_emit_event(pts, 0);
        s_was_down = false;
        if (s_no_touch_count < 255) s_no_touch_count++;
    }

    return (keep_polling || s_was_down || s_no_touch_count < settle || !has_int)
               ? interval : BSP_DISPATCH_IDLE;
}

/* ------------------------------------------------------------------------- */

void bsp_touch_set_active(bsp_touch_t *touch) {
    s_touch = touch;
    ensure_snapshot_lock();
    if (!touch) return;

    if (!s_wait_sem) s_wait_sem = xSemaphoreCreateBinary();

#ifdef ESP_PLATFORM
    attach_isr(touch);
#endif

    bsp_dispatch_add_source(&s_touch_source);
}
