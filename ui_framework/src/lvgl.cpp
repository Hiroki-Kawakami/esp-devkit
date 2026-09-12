/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "lvgl.hpp"
#include "sdkconfig.h"

#if CONFIG_HARNESS
#include "harness.h"
#endif

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

#if defined(ESP_PLATFORM) && CONFIG_HARNESS
static bool harness_idle(void *) {
    if (!lvgl_port_lock(0)) return false;
    bool idle = lv_anim_count_running() == 0;
    lvgl_port_unlock();
    return idle;
}

// Linked in place of esp_lvgl_port's lvgl_port_init() via -Wl,--wrap (added by
// CMakeLists.txt only when CONFIG_HARNESS): the app gets the harness without a
// call of its own, exactly like the simulator shim below.
extern "C" esp_err_t __real_lvgl_port_init(const lvgl_port_cfg_t *cfg);

extern "C" esp_err_t __wrap_lvgl_port_init(const lvgl_port_cfg_t *cfg) {
    esp_err_t err = __real_lvgl_port_init(cfg);
    if (err != ESP_OK) return err;
    harness_set_idle_cb(harness_idle, nullptr);
    harness_set_lock_cb([](void *) { lvgl_port_lock(0); },
                        [](void *) { lvgl_port_unlock(); }, nullptr);
    harness_start();
    return ESP_OK;
}
#endif

#ifndef ESP_PLATFORM
// Simulator-only LVGL "port" shim: mirror esp_lvgl_port's surface so app/board
// code calls lvgl_port_init() the same way on both targets. There is no LVGL
// task here — the host main thread drives LVGL via lvgl_sim_loop() below (SDL is
// main-thread-only, so the present loop must own the main thread).
#include <SDL2/SDL.h>
#include <unistd.h>
#include "sdl_panel.h"

static bool s_inited;

esp_err_t lvgl_port_init(const lvgl_port_cfg_t *cfg) {
    (void)cfg;   // task/stack/affinity knobs are meaningless without an LVGL task
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
