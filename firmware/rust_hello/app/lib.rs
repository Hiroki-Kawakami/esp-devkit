/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]
// CONFIG_BSP_RS_* exist only on targets that build bsp_rs.
#![allow(unexpected_cfgs)]

extern crate alloc;

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::Cell;
use core::ffi::CStr;
use core::time::Duration;

use idf::executor;
use idf::heap::{self, Caps};
use idf::log::{error, info};
use idf::sync::{LazyLock, Mutex, channel};
use idf::system::{CORE_COUNT, core_id};
use idf::thread::{self, Builder};
use idf::time::Instant;

mod net;

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
    #[cfg(CONFIG_BSP_RS_BOARD_TAB5)]
    let board = match bsp_rs::init(&bsp_rs::Config::default()) {
        Ok(board) => Some(board),
        Err(err) => {
            error!(TAG, "bsp_rs::init: {err}");
            None
        }
    };
    if let Err(err) = devkit_net::start(devkit_net::Config::default()) {
        error!(TAG, "devkit_net::start: {err}");
    }
    #[cfg(CONFIG_BSP_RS_BOARD_TAB5)]
    if let Some(board) = &board {
        net::register_commands();
        if let Err(err) = harness_rs::start(board) {
            error!(TAG, "harness_rs::start: {err}");
        }
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

    let (values, mut inbox) = executor::channel::<u32>(4);
    Builder::new()
        .name("rust_feeder")
        .spawn(move || {
            for i in 1..=5 {
                while values.try_send(i).is_err() {
                    thread::yield_now();
                }
                thread::sleep(Duration::from_millis(10));
            }
        })
        .expect("spawn");
    let shared = Builder::new().name("rust_async").spawn_executor().expect("executor");
    let jobs: Vec<_> = (1..=4u32)
        .map(|i| {
            shared.spawn(async move {
                executor::sleep(Duration::from_millis(u64::from(i) * 10)).await;
                (i * 2, core_id())
            })
        })
        .collect();

    executor::block_on(async move {
        let mut sum = 0;
        for _ in 0..5 {
            sum += inbox.recv().await;
        }
        let mut done = Vec::new();
        for job in jobs {
            done.push(job.await);
        }
        let hits = Rc::new(Cell::new(0));
        let local = executor::local_spawner().await;
        let counters: Vec<_> = (0..3)
            .map(|_| {
                let hits = hits.clone();
                local.spawn_local(async move { hits.set(hits.get() + 1) })
            })
            .collect();
        for counter in counters {
            counter.await;
        }
        info!(TAG, "async: channel sum={sum} jobs (value, core)={done:?} local hits={}", hits.get());
        info!(TAG, "uptime={}ms", Instant::now().since_boot().as_millis());

        net::serve_echo(&shared).await
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn weight_is_affine() {
        assert_eq!(super::rust_hello_weight(0), 1);
        assert_eq!(super::rust_hello_weight(255), 766);
    }
}
