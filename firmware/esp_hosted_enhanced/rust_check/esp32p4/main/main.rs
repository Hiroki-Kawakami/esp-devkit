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

use bsp_rs::sd::{MountConfig, Sd};
use idf::fs::{self, File};
use idf::heap::{self, Caps};
use idf::log::error;
use idf::time::Instant;

#[allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals)]
mod ffi {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

const TAG: &CStr = c"rust_check";

const SD_ROOT: &CStr = c"/sdcard";
const SD_FILE: &CStr = c"/sdcard/rust_check.bin";
const SD_CHUNK: usize = 64 * 1024;
const SD_DEFAULT_MB: usize = 16;
const REPLY_LEN: usize = 256;

static INT_FREE_AT_MAIN: AtomicUsize = AtomicUsize::new(0);

fn c_command(name: &'static str, args: &[&str]) -> Result<Option<String>, String> {
    let owned: Vec<CString> = core::iter::once(name)
        .chain(args.iter().copied())
        .map(|a| CString::new(a).map_err(|_| "bad argument".to_string()))
        .collect::<Result<_, _>>()?;
    let argv: Vec<*const c_char> = owned.iter().map(|a| a.as_ptr()).collect();
    let mut reply = [0u8; REPLY_LEN];
    let ok = unsafe { ffi::check_c_command(argv.len() as i32, argv.as_ptr(), reply.as_mut_ptr().cast(), REPLY_LEN) };
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

fn fill(chunk: &mut [u8], index: usize) {
    for (i, word) in chunk.chunks_exact_mut(4).enumerate() {
        let v = (index * SD_CHUNK / 4 + i) as u32 ^ 0x5a5a_5a5a;
        word.copy_from_slice(&v.to_le_bytes());
    }
}

fn kbps(bytes: usize, us: u128) -> u128 {
    if us == 0 { 0 } else { bytes as u128 * 1_000_000 / 1024 / us }
}

fn sd_bench(sd: &Sd, args: &[&str]) -> Result<Option<String>, String> {
    let mb = args.first().map_or(Ok(SD_DEFAULT_MB), |a| a.parse()).map_err(|_| "bad size".to_string())?;
    let chunks = mb * 1024 * 1024 / SD_CHUNK;
    if !sd.is_mounted() {
        sd.mount(SD_ROOT, &MountConfig::default()).map_err(|e| format!("mount: {e}"))?;
    }
    let mut buf = heap::try_vec::<u8>(SD_CHUNK, Caps::INTERNAL | Caps::DMA).map_err(|e| format!("{e}"))?;
    buf.resize(SD_CHUNK, 0);

    let t0 = Instant::now();
    let file = File::create(SD_FILE).map_err(|e| format!("create: {e}"))?;
    for i in 0..chunks {
        fill(&mut buf, i);
        file.write_all(&buf).map_err(|e| format!("write: {e}"))?;
    }
    file.sync().map_err(|e| format!("sync: {e}"))?;
    drop(file);
    let write_us = t0.elapsed().as_micros();

    let mut expect = alloc::vec![0u8; SD_CHUNK];
    let t0 = Instant::now();
    let file = File::open(SD_FILE).map_err(|e| format!("open: {e}"))?;
    for i in 0..chunks {
        let mut got = 0;
        while got < SD_CHUNK {
            let n = file.read(&mut buf[got..]).map_err(|e| format!("read: {e}"))?;
            if n == 0 {
                return Err(format!("short file at chunk {i}"));
            }
            got += n;
        }
        fill(&mut expect, i);
        if buf[..] != expect[..] {
            return Err(format!("mismatch in chunk {i}"));
        }
    }
    let read_us = t0.elapsed().as_micros();
    drop(file);
    let _ = fs::remove_file(SD_FILE);
    let bytes = chunks * SD_CHUNK;
    Ok(Some(format!("{} {}", kbps(bytes, write_us), kbps(bytes, read_us))))
}

#[unsafe(no_mangle)]
pub extern "C" fn app_main() {
    let board = match bsp_rs::init(&bsp_rs::Config::default()) {
        Ok(board) => board,
        Err(err) => {
            error!(TAG, "bsp_rs::init: {err}");
            return;
        }
    };
    unsafe { ffi::check_start() };
    for name in ["wifi", "ble"] {
        harness_rs::register(name, move |args| c_command(name, args));
    }
    harness_rs::register("heap", |_| Ok(Some(heap_figures())));
    if let Some(sd) = board.sd {
        harness_rs::register("sd", move |args| sd_bench(sd, args));
    }
    if let Err(err) = harness_rs::start(&board) {
        error!(TAG, "harness: {err}");
    }
    INT_FREE_AT_MAIN.store(heap::free_size(Caps::INTERNAL), Ordering::Relaxed);
    unsafe { ffi::check_wifi_autoconnect() };
}
