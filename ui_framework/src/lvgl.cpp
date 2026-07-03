/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "lvgl.hpp"

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
    return ESP_OK;
}

// Run LVGL until `tick` returns false: pump input, service timers, present, then
// hand the frame to the caller (the sim-harness step), passing whether the UI is
// idle (no animation running) for the harness `settle` predicate.
void lvgl_sim_loop(std::function<bool(bool is_idle)> tick) {
    while (s_inited) {
        sdl_panel_pump_input();
        uint32_t sleep_time_ms = lv_timer_handler();
        if (sleep_time_ms == LV_NO_TIMER_READY) sleep_time_ms = LV_DEF_REFR_PERIOD;
        sdl_panel_present();
        if (!tick(lv_anim_count_running() == 0)) break;
        usleep(sleep_time_ms * 1000);
    }
}
#endif
