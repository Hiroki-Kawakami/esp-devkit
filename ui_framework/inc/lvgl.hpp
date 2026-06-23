/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * lvgl++ — small C++ conveniences over the LVGL C API: std::function-friendly
 * wrappers for async calls and event handlers (closures/captures, with the
 * lifetime managed for you). Target-agnostic; works against either LVGL build.
 *
 * This header also fronts the LVGL "port": on device it pulls in the real
 * <esp_lvgl_port.h>; on the simulator it declares a same-signature shim
 * (lvgl_port_cfg_t + lvgl_port_init(), implemented in lvgl.cpp) plus
 * lvgl_sim_loop() — the host present loop. App/board code thus calls
 * lvgl_port_init() identically on both targets.
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

#ifdef ESP_PLATFORM
#include "esp_lvgl_port.h"
#else
typedef struct {
    int task_priority;        /*!< LVGL task priority */
    int task_stack;           /*!< LVGL task stack size */
    int task_affinity;        /*!< LVGL task pinned to core (-1 is no affinity) */
    int task_max_sleep_ms;    /*!< Maximum sleep in LVGL task */
    unsigned task_stack_caps; /*!< LVGL task stack memory capabilities (see esp_heap_caps.h) */
    int timer_period_ms;      /*!< LVGL timer tick period in ms */
} lvgl_port_cfg_t;

esp_err_t lvgl_port_init(const lvgl_port_cfg_t *cfg);
void lvgl_sim_loop(std::function<bool(bool is_idle)> tick);
#endif

#include "widgets/layout.hpp"
