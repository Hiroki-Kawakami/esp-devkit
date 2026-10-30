/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![no_std]

extern crate alloc;

mod bench;
mod scene;

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use core::ffi::CStr;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};

use bsp_rs::display::PixelFormat;
use idf::log::{error, info};
use idf::sync::{Mutex, Semaphore};
use idf::{system, thread};

use bench::{Bench, Params, Present};
use scene::Kind;

const TAG: &CStr = c"ui_bench";
const FORMAT_888: u32 = 0x0888_0888;

#[cfg_attr(target_os = "none", unsafe(link_section = ".rtc_noinit.ui_bench"))]
static mut FORMAT: MaybeUninit<u32> = MaybeUninit::uninit();

fn format() -> PixelFormat {
    let value = unsafe { (&raw const FORMAT).cast::<u32>().read_volatile() };
    if value == FORMAT_888 { PixelFormat::Rgb888 } else { PixelFormat::Rgb565 }
}

fn set_format(value: u32) {
    unsafe { (&raw mut FORMAT).cast::<u32>().write_volatile(value) };
}

fn parse(args: &[&str]) -> Result<Params, String> {
    let kind = match args.first().copied() {
        Some("list") => Kind::List,
        Some("thumbs") => Kind::Thumbs,
        Some("dense") => Kind::Dense,
        Some("menu") => Kind::Menu,
        Some("tick") => Kind::Tick,
        _ => return Err("bench <list|thumbs|dense|menu|tick> <inplace|swap2|swap3> <rot0|rot90> <lanes> [frames] [speed] [psram|sram]".to_string()),
    };
    let present = match args.get(1).copied() {
        Some("inplace") => Present::InPlace,
        Some("swap2") => Present::Swap2,
        Some("swap3") => Present::Swap3,
        _ => return Err("bad present mode".to_string()),
    };
    let rotated = match args.get(2).copied() {
        Some("rot0") => false,
        Some("rot90") => true,
        _ => return Err("bad rotation".to_string()),
    };
    let lanes = args.get(3).and_then(|v| v.parse().ok()).filter(|n| (1..=2).contains(n));
    let lanes = lanes.ok_or("bad lane count")?;
    let frames = args.get(4).and_then(|v| v.parse().ok()).unwrap_or(240);
    let speed = args.get(5).and_then(|v| v.parse().ok()).unwrap_or(7);
    let cache_in_psram = args.get(6).copied() != Some("sram");
    Ok(Params { kind, present, rotated, lanes, frames, speed, cache_in_psram })
}

pub fn run() {
    let mut config = bsp_rs::Config::default();
    config.display.fb_num = 3;
    config.display.pixel_format = Some(format());
    let board = match bsp_rs::init(&config) {
        Ok(board) => board,
        Err(err) => {
            error!(TAG, "bsp_rs::init: {err}");
            return;
        }
    };
    let Some(display) = board.display else {
        error!(TAG, "no display");
        return;
    };
    if let Err(err) = display.present(0) {
        error!(TAG, "present: {err}");
    }
    if let Err(err) = display.set_brightness(100) {
        error!(TAG, "set_brightness: {err}");
    }
    let mut bench = match Bench::new(display) {
        Ok(bench) => bench,
        Err(err) => {
            error!(TAG, "bench: {err}");
            return;
        }
    };
    info!(TAG, "ready {:?}, internal free {}", display.pixel_format(), idf::heap::free_size(idf::heap::Caps::DMA | idf::heap::Caps::INTERNAL));

    let running = Arc::new(AtomicBool::new(false));
    let result = Arc::new(Mutex::new(String::new()));
    let request = Arc::new(Mutex::new(None::<Params>));
    let wake = Arc::new(Semaphore::new(0, 1));

    {
        let (running, request, wake) = (running.clone(), request.clone(), wake.clone());
        harness_rs::register("bench", move |args| {
            if running.swap(true, Ordering::AcqRel) {
                return Err("busy".to_string());
            }
            match parse(args) {
                Ok(params) => {
                    *request.lock() = Some(params);
                    wake.give();
                    Ok(None)
                }
                Err(err) => {
                    running.store(false, Ordering::Release);
                    Err(err)
                }
            }
        });
    }
    {
        let result = result.clone();
        harness_rs::register("bench-result", move |_| Ok(Some(result.lock().clone())));
    }
    harness_rs::register("bench-format", move |args| {
        match args.first().copied() {
            Some("565") => set_format(0),
            Some("888") => set_format(FORMAT_888),
            _ => return Err("bench-format <565|888>".to_string()),
        }
        thread::spawn(|| {
            thread::sleep(core::time::Duration::from_millis(100));
            system::restart();
        });
        Ok(None)
    });
    {
        let running = running.clone();
        harness_rs::set_idle(move || !running.load(Ordering::Acquire));
    }
    if let Err(err) = harness_rs::start(&board) {
        error!(TAG, "harness: {err}");
    }

    loop {
        wake.take();
        let Some(params) = request.lock().take() else { continue };
        let line = bench.run(&params);
        info!(TAG, "{line}");
        *result.lock() = line;
        running.store(false, Ordering::Release);
    }
}
