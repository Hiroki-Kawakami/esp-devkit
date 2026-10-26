/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use core::ffi::c_int;

mod ffi {
    #![allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals)]
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

fn log(line: &str) {
    unsafe { ffi::printf(c"%.*s\n".as_ptr(), line.len() as c_int, line.as_ptr()) };
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_hello_weight(byte: u8) -> u32 {
    u32::from(byte) * 3 + 1
}

pub fn run() {
    let err = unsafe { ffi::bsp_init(core::ptr::null()) };
    if err != 0 {
        log(&format!("rust_hello: bsp_init failed: {err}"));
    }

    let bytes: Vec<u8> = (0..=255).collect();
    let checksum = unsafe { ffi::rust_hello_checksum(bytes.as_ptr(), bytes.len()) };
    let wide: Vec<u64> = (1..=8).map(|i| u64::MAX / i).collect();
    let quotient = u128::from(wide[2]) * 1_000_000_007 / 13;
    let third = 1.0f64 / 3.0;
    log(&format!(
        "rust_hello: checksum={checksum:08x} quotient={quotient} third={third:.9} align={}",
        wide.as_ptr() as usize % 8
    ));
    #[cfg(CONFIG_SPIRAM)]
    log("rust_hello: PSRAM enabled");

    let mut seconds = 0u32;
    loop {
        log(&format!(
            "rust_hello: {seconds}s free={}",
            unsafe { ffi::esp_get_free_heap_size() }
        ));
        seconds += 1;
        unsafe { ffi::vTaskDelay(sdkconfig::CONFIG_FREERTOS_HZ as u32) };
    }
}
