/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]

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
    let config: ffi::bsp_config_t = unsafe { core::mem::zeroed() };
    let err = unsafe { ffi::bsp_init(&config) };
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
            "rust_hello: {seconds}s uptime={}ms",
            unsafe { ffi::esp_timer_get_time() } / 1000
        ));
        seconds += 1;
        unsafe { ffi::vTaskDelay(ffi::rust_hello_ms_to_ticks(1000)) };
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn weight_is_affine() {
        assert_eq!(super::rust_hello_weight(0), 1);
        assert_eq!(super::rust_hello_weight(255), 766);
    }

    #[test]
    fn uptime_advances() {
        let start = unsafe { super::ffi::esp_timer_get_time() };
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(unsafe { super::ffi::esp_timer_get_time() } - start >= 2000);
    }
}
