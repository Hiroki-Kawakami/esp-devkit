/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * lvgl++ — small C++ conveniences over the LVGL C API: std::function-friendly
 * wrappers for async calls and event handlers (closures/captures, with the
 * lifetime managed for you).
 *
 * This header also fronts the LVGL port: lvgl_port_init() runs LVGL in its own
 * task on device and on the host main thread (lvgl_sim_loop()) on the simulator.
 *
 * With CONFIG_HARNESS the test harness (libs/harness) is wired to this port
 * from inside lvgl_port_init() -- idle = no running animation, captures
 * serialized against the LVGL task -- so the app needs no call of its own.
 */

#pragma once
#include <functional>
#include "lvgl.h"
#include "esp_err.h"

/* Run `fn` once on the LVGL context (next lv_timer_handler tick). */
lv_result_t lv_async_call(std::function<void()> fn);

/* lv_obj_add_event_cb that takes a std::function. The closure is owned by the
 * object and freed on LV_EVENT_DELETE. */
lv_event_dsc_t *lv_obj_add_event_fn(lv_obj_t *obj, lv_event_code_t filter,
                                    std::function<void(lv_event_t *)> fn);

/* The task_* fields are ignored on the simulator, and timer_period_ms
 * everywhere: the tick is read from the system clock. */
typedef struct {
    int task_priority;
    int task_stack;
    int task_affinity;        /* -1: no affinity */
    int task_max_sleep_ms;    /* 0: 500 ms */
    unsigned task_stack_caps; /* 0: internal RAM */
    int timer_period_ms;
} lvgl_port_cfg_t;

/* Initializes LVGL. Take lv_lock() around LVGL calls made outside the LVGL
 * context. */
esp_err_t lvgl_port_init(const lvgl_port_cfg_t *cfg);

#ifndef ESP_PLATFORM
/* Host present loop; returns once the harness receives `quit` (or stdin EOF). */
void lvgl_sim_loop();
#endif
