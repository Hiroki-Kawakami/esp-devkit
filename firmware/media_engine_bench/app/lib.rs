/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![no_std]

use core::ffi::CStr;

use idf::log::info;

const TAG: &CStr = c"media_engine_bench";

pub fn run() {
    info!(TAG, "nothing to run yet");
}
