/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host simulator entry for core_s3_hello. The host main thread owns the present
 * loop (lvgl_sim_loop), stepping the optional scripted sim harness once per frame.
 */

#include "core_s3_hello.hpp"
#include "sim_harness.h"
#include "lvgl.hpp"
#include <cstdlib>

extern "C" int main(void) {
    app_entry();

    sim_harness_start(getenv("SIMULATOR_SCRIPT"));
    lvgl_sim_loop(sim_harness_frame);
    return sim_harness_exit_code();
}
