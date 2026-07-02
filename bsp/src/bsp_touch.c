/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Common touch layer: dispatches the public bsp_touch_* API through the active
 * chip vtable and owns the shared reader task, INT ISR, orientation transform,
 * and cached snapshot. Chip drivers only implement poll()/deinit().
 *
 * IDLE is a capability that needs an INT line: with one, the task sleeps on
 * the shared sem after settle_count consecutive no-finger polls and the ISR
 * wakes it. Boards that leave INT NC (e.g. s31_korvo) never idle -- the tick
 * stays ACTIVE and the task just polls every poll_interval, matching the
 * pre-refactor behaviour of the chip-owned reader tasks.
 */

#include "bsp.h"
#include "bsp_touch.h"

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
#define DEFAULT_POLL_INTERVAL_MS   10
#define DEFAULT_TASK_STACK         3072

static bsp_touch_t         *s_touch;
static bsp_touch_event_cb_t s_event_cb;
static void                *s_event_arg;

static SemaphoreHandle_t s_snapshot_lock;
static bsp_touch_point_t s_snapshot[BSP_TOUCH_MAX_POINTS];
static uint8_t           s_snapshot_count;

/* s_int_sem: reader task; s_wait_sem: bsp_touch_wait_interrupt callers. Both
 * given by the ISR (device) / bsp_touch_notify (simulator). */
static SemaphoreHandle_t s_int_sem;
static SemaphoreHandle_t s_wait_sem;

static TaskHandle_t      s_task;
static SemaphoreHandle_t s_task_done;
static volatile bool     s_task_stop;
static bool              s_has_task;
static TickType_t        s_poll_interval;

static uint8_t s_no_touch_count;
static bool    s_was_down;

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
    if (s_int_sem)  xSemaphoreGive(s_int_sem);
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

    /* No task: refresh the snapshot from the chip on demand. */
    if (!s_has_task && s_touch->poll) {
        bsp_touch_raw_point_t raw[BSP_TOUCH_MAX_POINTS];
        uint8_t max = s_touch->max_points ? s_touch->max_points : BSP_TOUCH_MAX_POINTS;
        if (max > BSP_TOUCH_MAX_POINTS) max = BSP_TOUCH_MAX_POINTS;
        uint8_t n = 0; bool keep = false;
        if (s_touch->poll(s_touch, raw, max, &n, &keep) == ESP_OK) {
            bsp_touch_point_t pts[BSP_TOUCH_MAX_POINTS];
            apply_orientation(s_touch, raw, pts, n);
            bsp_touch_emit_event(pts, n);
        }
    }

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
    if (s_int_sem)  xSemaphoreGiveFromISR(s_int_sem,  &hp);
    if (s_wait_sem) xSemaphoreGiveFromISR(s_wait_sem, &hp);
    if (hp) portYIELD_FROM_ISR();
}

static void attach_isr(bsp_touch_t *t) {
    if (!t || t->int_io < 0 || s_int_isr_attached) return;
    esp_err_t err = gpio_install_isr_service(0);
    if (err != ESP_OK && err != ESP_ERR_INVALID_STATE) {
        ESP_LOGW(TAG, "gpio_install_isr_service: %s", esp_err_to_name(err));
        return;
    }
    gpio_set_intr_type((gpio_num_t)t->int_io, GPIO_INTR_NEGEDGE);
    if (gpio_isr_handler_add((gpio_num_t)t->int_io, touch_int_isr, t) != ESP_OK) return;
    gpio_intr_enable((gpio_num_t)t->int_io);
    s_int_isr_attached = true;
}
#endif  /* ESP_PLATFORM */

static bool touch_tick(void) {
    bsp_touch_t *t = s_touch;
    if (!t || !t->poll) return false;

    const uint8_t settle  = t->settle_count ? t->settle_count : DEFAULT_SETTLE_COUNT;
    const bool    has_int = (t->int_io >= 0);

    if (has_int && !t->int_pending && s_no_touch_count >= settle && !s_was_down)
        return false;
    t->int_pending = false;

    bsp_touch_raw_point_t raw[BSP_TOUCH_MAX_POINTS];
    uint8_t max = t->max_points ? t->max_points : BSP_TOUCH_MAX_POINTS;
    if (max > BSP_TOUCH_MAX_POINTS) max = BSP_TOUCH_MAX_POINTS;
    uint8_t n = 0;
    bool keep_polling = false;

    if (t->poll(t, raw, max, &n, &keep_polling) != ESP_OK) return true;

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

    return keep_polling || s_was_down || s_no_touch_count < settle || !has_int;
}

static void touch_task(void *arg) {
    (void)arg;
    while (!s_task_stop) {
        bool active = touch_tick();
        xSemaphoreTake(s_int_sem, active ? s_poll_interval : portMAX_DELAY);
    }
    if (s_task_done) xSemaphoreGive(s_task_done);
    vTaskDelete(NULL);
}

/* ------------------------------------------------------------------------- */

void bsp_touch_set_active(bsp_touch_t *touch) {
    s_touch = touch;
    ensure_snapshot_lock();
    if (!touch) return;

    if (!s_int_sem)  s_int_sem  = xSemaphoreCreateBinary();
    if (!s_wait_sem) s_wait_sem = xSemaphoreCreateBinary();

#ifdef ESP_PLATFORM
    attach_isr(touch);
#endif
}

esp_err_t bsp_touch_start_reader(uint8_t priority, int8_t affinity,
                                 uint32_t poll_interval_ms, uint32_t task_stack) {
    if (priority == 0) return ESP_OK;                        /* opted out */
    if (!s_touch)      return ESP_ERR_INVALID_STATE;
    if (s_has_task)    return ESP_ERR_INVALID_STATE;
    if (!s_int_sem)    s_int_sem = xSemaphoreCreateBinary();
    if (!s_int_sem)    return ESP_ERR_NO_MEM;

    s_task_done = xSemaphoreCreateBinary();
    if (!s_task_done) return ESP_ERR_NO_MEM;

    s_poll_interval = pdMS_TO_TICKS(poll_interval_ms ? poll_interval_ms : DEFAULT_POLL_INTERVAL_MS);
    BaseType_t core = affinity < 0 ? tskNO_AFFINITY : (BaseType_t)affinity;
    uint32_t stack = task_stack ? task_stack : DEFAULT_TASK_STACK;
    if (xTaskCreatePinnedToCore(touch_task, "bsp_touch", stack, NULL,
                                priority, &s_task, core) != pdPASS) {
        vSemaphoreDelete(s_task_done);
        s_task_done = NULL;
        return ESP_FAIL;
    }
    s_has_task = true;
    return ESP_OK;
}

bool bsp_touch_reader_running(void) {
    return s_has_task;
}
