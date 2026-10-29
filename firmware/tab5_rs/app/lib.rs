/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![no_std]

use core::ffi::CStr;

use idf::log::{error, info};

const TAG: &CStr = c"tab5_rs";

pub fn run() {
    match bsp_rs::init(&bsp_rs::Config::default()) {
        Ok(_board) => info!(TAG, "bsp_rs ready"),
        Err(err) => error!(TAG, "bsp_rs::init failed: {err}"),
    }
}
