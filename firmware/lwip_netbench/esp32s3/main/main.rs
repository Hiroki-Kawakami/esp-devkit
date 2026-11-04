/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![no_std]

extern crate alloc;

use alloc::ffi::CString;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::{CStr, c_char};
use core::sync::atomic::{AtomicUsize, Ordering};

use idf::heap::{self, Caps};
use idf::log::error;

#[allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals)]
mod ffi {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

const TAG: &CStr = c"lwip_netbench";
const REPLY_LEN: usize = 256;

static INT_FREE_AT_MAIN: AtomicUsize = AtomicUsize::new(0);

fn c_command(name: &'static str, args: &[&str]) -> Result<Option<String>, String> {
    let owned: Vec<CString> = core::iter::once(name)
        .chain(args.iter().copied())
        .map(|a| CString::new(a).map_err(|_| "bad argument".to_string()))
        .collect::<Result<_, _>>()?;
    let argv: Vec<*const c_char> = owned.iter().map(|a| a.as_ptr()).collect();
    let mut reply = [0u8; REPLY_LEN];
    let ok = unsafe { ffi::bench_c_command(argv.len() as i32, argv.as_ptr(), reply.as_mut_ptr().cast(), REPLY_LEN) };
    let reply = CStr::from_bytes_until_nul(&reply).map_or("", |r| r.to_str().unwrap_or(""));
    let body = |prefix: &str| {
        reply.strip_prefix(prefix).map(|rest| rest.strip_prefix(name).unwrap_or(rest).trim_start().to_string())
    };
    if let Some(err) = body("ERR ") {
        return Err(err);
    }
    if !ok {
        return Err(String::new());
    }
    Ok(body("OK ").filter(|text| !text.is_empty()))
}

fn heap_figures() -> String {
    format!(
        "{} {} {} {} {} {}",
        heap::free_size(Caps::INTERNAL),
        heap::minimum_free_size(Caps::INTERNAL),
        heap::largest_free_block(Caps::INTERNAL),
        heap::free_size(Caps::INTERNAL | Caps::DMA),
        heap::free_size(Caps::SPIRAM),
        INT_FREE_AT_MAIN.load(Ordering::Relaxed)
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn app_main() {
    INT_FREE_AT_MAIN.store(heap::free_size(Caps::INTERNAL), Ordering::Relaxed);
    let board = match bsp_rs::init(&bsp_rs::Config::default()) {
        Ok(board) => board,
        Err(err) => {
            error!(TAG, "bsp_rs::init: {err}");
            return;
        }
    };
    unsafe { ffi::bench_start() };
    harness_rs::register("wifi", |args| c_command("wifi", args));
    harness_rs::register("heap", |_| Ok(Some(heap_figures())));
    if let Err(err) = harness_rs::start(&board) {
        error!(TAG, "harness: {err}");
    }
    unsafe { ffi::bench_wifi_autoconnect() };
}
