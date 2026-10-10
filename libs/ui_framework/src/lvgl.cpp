/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "lvgl.hpp"
#include "sdkconfig.h"

#if CONFIG_HARNESS
#include "harness.h"
#endif

extern "C" __attribute__((weak)) void resgen_resources_init(void) {}

lv_result_t lv_async_call(std::function<void()> fn) {
    auto *fn_ptr = new std::function<void()>(std::move(fn));
    auto result = lv_async_call([](void *arg) {
        auto *fn = static_cast<std::function<void()>*>(arg);
        (*fn)();
        delete fn;
    }, fn_ptr);
    if (result != LV_RESULT_OK) delete fn_ptr;
    return result;
}

lv_event_dsc_t *lv_obj_add_event_fn(lv_obj_t *obj, lv_event_code_t filter,
                                    std::function<void(lv_event_t*)> fn) {
    auto *fn_ptr = new std::function<void(lv_event_t*)>(std::move(fn));

    lv_obj_add_event_cb(obj, [](lv_event_t *e) {
        delete static_cast<std::function<void(lv_event_t*)>*>(lv_event_get_user_data(e));
    }, LV_EVENT_DELETE, fn_ptr);

    return lv_obj_add_event_cb(obj, [](lv_event_t *e) {
        auto fn = *static_cast<std::function<void(lv_event_t*)>*>(lv_event_get_user_data(e));
        fn(e);
    }, filter, fn_ptr);
}

#ifdef ESP_PLATFORM
#include "esp_heap_caps.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "freertos/task.h"
#include "freertos/idf_additions.h"

// Not a task notification: with LV_USE_FREERTOS_TASK_NOTIFY, LVGL's own
// thread sync waits on the LVGL task's notification slot, and a stray wake
// would end that wait early.
static SemaphoreHandle_t s_wake;

// Linked in place of LVGL's lv_async_call() via -Wl,--wrap (see CMakeLists.txt):
// the LVGL task sleeps until its next timer, so a queued async call would not
// run until then without this wake.
extern "C" lv_result_t __real_lv_async_call(lv_async_cb_t async_xcb, void *user_data);

extern "C" lv_result_t __wrap_lv_async_call(lv_async_cb_t async_xcb, void *user_data) {
    lv_result_t res = __real_lv_async_call(async_xcb, user_data);
    if (res == LV_RESULT_OK && s_wake) xSemaphoreGive(s_wake);
    return res;
}

#if CONFIG_HARNESS
static bool harness_idle(void *) {
    lv_lock();
    bool idle = lv_anim_count_running() == 0;
    lv_unlock();
    return idle;
}
#endif

static uint32_t tick_ms() {
    return static_cast<uint32_t>(esp_timer_get_time() / 1000);
}

static void lvgl_task(void *arg) {
    const uint32_t max_sleep_ms = reinterpret_cast<uintptr_t>(arg);
    while (true) {
        uint32_t sleep_ms = lv_display_get_default() ? lv_timer_handler() : 0;
        if (sleep_ms > max_sleep_ms) sleep_ms = max_sleep_ms;
        TickType_t ticks = pdMS_TO_TICKS(sleep_ms);
        xSemaphoreTake(s_wake, ticks > 0 ? ticks : 1);
    }
}

esp_err_t lvgl_port_init(const lvgl_port_cfg_t *cfg) {
    if (!cfg || cfg->task_affinity >= configNUM_CORES) return ESP_ERR_INVALID_ARG;
    if (s_wake) return ESP_ERR_INVALID_STATE;

    resgen_resources_init();
    lv_init();
    lv_tick_set_cb(tick_ms);

    s_wake = xSemaphoreCreateBinary();
    if (!s_wake) return ESP_ERR_NO_MEM;

    const uintptr_t max_sleep_ms = cfg->task_max_sleep_ms > 0 ? cfg->task_max_sleep_ms : 500;
    const UBaseType_t caps = cfg->task_stack_caps ? cfg->task_stack_caps
                                                  : MALLOC_CAP_INTERNAL | MALLOC_CAP_DEFAULT;
    const BaseType_t core = cfg->task_affinity < 0 ? tskNO_AFFINITY : cfg->task_affinity;
    if (xTaskCreatePinnedToCoreWithCaps(lvgl_task, "taskLVGL", cfg->task_stack,
                                        reinterpret_cast<void *>(max_sleep_ms),
                                        cfg->task_priority, nullptr, core, caps) != pdPASS) {
        vSemaphoreDelete(s_wake);
        s_wake = nullptr;
        return ESP_FAIL;
    }

#if CONFIG_HARNESS
    harness_set_idle_cb(harness_idle, nullptr);
    harness_set_lock_cb([](void *) { lv_lock(); }, [](void *) { lv_unlock(); }, nullptr);
    harness_start();
#endif
    return ESP_OK;
}
#endif

#ifndef ESP_PLATFORM
// No LVGL task on the simulator: the host main thread drives LVGL via
// lvgl_sim_loop() below (SDL is main-thread-only, so the present loop must own
// the main thread).
#include <SDL2/SDL.h>
#include <unistd.h>
#include "sdl_panel.h"

static bool s_inited;

esp_err_t lvgl_port_init(const lvgl_port_cfg_t *cfg) {
    (void)cfg;   // task/stack/affinity knobs are meaningless without an LVGL task
    resgen_resources_init();
    lv_init();
    lv_tick_set_cb(SDL_GetTicks);
    lv_delay_set_cb(SDL_Delay);

    s_inited = true;
    harness_start();
    return ESP_OK;
}

// Run LVGL until the harness asks to quit: pump input, service timers, present,
// then publish whether the UI is idle (no animation running) for the harness
// `idle` command.
//
// lv_timer_handler()'s sleep hint covers LVGL's own timers and nothing else, so
// it goes idle for hundreds of milliseconds while a framebuffer written from
// outside LVGL — video, camera, any DMA-style producer — already holds new
// content. The present cadence is therefore capped independently of it;
// sdl_panel_present() returns immediately when the panel is not dirty, so the
// extra wakeups cost a timer scan.
static const uint32_t kPresentIntervalMs = 16;

void lvgl_sim_loop() {
    while (s_inited) {
        sdl_panel_pump_input();
        uint32_t sleep_time_ms = lv_timer_handler();
        if (sleep_time_ms == LV_NO_TIMER_READY) sleep_time_ms = LV_DEF_REFR_PERIOD;
        if (sleep_time_ms > kPresentIntervalMs) sleep_time_ms = kPresentIntervalMs;
        sdl_panel_present();
        if (!harness_frame(lv_anim_count_running() == 0)) break;
        usleep(sleep_time_ms * 1000);
    }
}
#endif
