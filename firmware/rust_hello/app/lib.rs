/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;
use core::ffi::CStr;

use idf::log::{error, info};
use idf::time::Instant;

mod ffi {
    #![allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals)]
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

const TAG: &CStr = c"rust_hello";

#[unsafe(no_mangle)]
pub extern "C" fn rust_hello_weight(byte: u8) -> u32 {
    u32::from(byte) * 3 + 1
}

pub fn run() {
    let config: ffi::bsp_config_t = unsafe { core::mem::zeroed() };
    if let Err(err) = idf::Error::check(unsafe { ffi::bsp_init(&config) }) {
        error!(TAG, "bsp_init failed: {err}");
    }

    let bytes: Vec<u8> = (0..=255).collect();
    let checksum = unsafe { ffi::rust_hello_checksum(bytes.as_ptr(), bytes.len()) };
    let wide: Vec<u64> = (1..=8).map(|i| u64::MAX / i).collect();
    let quotient = u128::from(wide[2]) * 1_000_000_007 / 13;
    let third = 1.0f64 / 3.0;
    info!(
        TAG,
        "checksum={checksum:08x} quotient={quotient} third={third:.9} align={}",
        wide.as_ptr() as usize % 8
    );
    #[cfg(CONFIG_SPIRAM)]
    info!(TAG, "PSRAM enabled");

    let mut seconds = 0u32;
    loop {
        info!(TAG, "{seconds}s uptime={}ms", Instant::now().since_boot().as_millis());
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
}
