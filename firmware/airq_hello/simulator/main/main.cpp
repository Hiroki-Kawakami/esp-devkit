/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host simulator entry for airq_hello. Mirrors the other LVGL sample sims: the
 * host main thread owns the present loop (lvgl_sim_loop); the harness started
 * in app_entry drives it over stdio.
 */

#include "airq_hello.hpp"
#include "lvgl.hpp"

extern "C" int main(void) {
    app_entry();
    lvgl_sim_loop();
    return 0;
}
