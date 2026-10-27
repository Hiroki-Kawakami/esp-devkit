/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;
use core::ffi::CStr;
use core::time::Duration;

use idf::heap::{self, Caps};
use idf::log::{error, info};
use idf::sync::{LazyLock, Mutex, channel};
use idf::system::{CORE_COUNT, core_id};
use idf::thread::{self, Builder};
use idf::time::Instant;

mod ffi {
    #![allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals)]
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

const TAG: &CStr = c"rust_hello";

static TOTAL: LazyLock<Mutex<u32>> = LazyLock::new(|| Mutex::new(0));

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

    let free = heap::free_size(Caps::DMA);
    let dma: Vec<u8, Caps> = Vec::with_capacity_in(4096, Caps::DMA | Caps::INTERNAL);
    info!(TAG, "dma buffer at {:p}, free DMA {free} -> {}", dma.as_ptr(), heap::free_size(Caps::DMA));

    let (tx, rx) = channel::<u32>(2);
    let workers: Vec<_> = (1..=2)
        .map(|id| {
            let tx = tx.clone();
            Builder::new()
                .name("rust_worker")
                .spawn(move || {
                    for i in 1..=10 {
                        *TOTAL.lock() += i * id;
                    }
                    tx.send(id);
                    id * 100
                })
                .expect("spawn")
        })
        .collect();
    let ids = [rx.recv(), rx.recv()];
    let results: Vec<u32> = workers.into_iter().map(|w| w.join()).collect();
    info!(TAG, "workers: total={} ids={ids:?} results={results:?}", *TOTAL.lock());
    let me = thread::current();
    info!(TAG, "task {} on core {}/{CORE_COUNT}, stack high water mark {}", me.name(), core_id(),
          me.stack_high_water_mark());

    let mut seconds = 0u32;
    loop {
        info!(TAG, "{seconds}s uptime={}ms", Instant::now().since_boot().as_millis());
        seconds += 1;
        thread::sleep(Duration::from_secs(1));
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
