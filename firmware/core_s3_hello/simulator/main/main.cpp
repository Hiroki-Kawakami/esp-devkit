/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host simulator entry for core_s3_hello. The host main thread owns the present
 * loop (lvgl_sim_loop); the harness started in app_entry drives it over stdio.
 */

#include "core_s3_hello.hpp"
#include "lvgl.hpp"

extern "C" int main(void) {
    app_entry();
    lvgl_sim_loop();
    return 0;
}
