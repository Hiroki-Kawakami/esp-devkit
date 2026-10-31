/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![no_std]

extern crate alloc;

mod bench;
mod demo;
mod scene;

mod resources {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/resources.rs"));
}

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use core::ffi::CStr;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use bsp_rs::Board;
use bsp_rs::display::{Display, PixelFormat};
use devkit::StaticLease;
use devkit_ui::{App, AppConfig, Insets, Rotation, Strips, TextInput};
use idf::heap::Caps;
use idf::log::{error, info};
use idf::sync::{Mutex, Semaphore};
use idf::{system, thread};

use bench::{Bench, Params, Present};
use demo::{FileManager, INSET_TOP, LIGHT, SPARE_STRIPS, StatusBar, StatusMsg};
use scene::Kind;

const TAG: &CStr = c"ui_demo";
const SETTINGS_VALID: u32 = 0x5549_4445;
const STRIP_BYTES: usize = 4 * 64 * 1024;
const VIDEO_STRIP_BYTES: usize = 2 * 64 * 1024;

#[repr(C, align(64))]
struct SharedSram([u8; STRIP_BYTES]);

impl AsMut<[u8]> for SharedSram {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

static SRAM: StaticLease<SharedSram> = StaticLease::new(SharedSram([0; STRIP_BYTES]));

#[derive(Clone, Copy)]
#[repr(C)]
struct Settings {
    valid: u32,
    rgb888: bool,
    bench: bool,
    rotated: bool,
    data_internal: bool,
    stack_psram: bool,
}

#[cfg_attr(target_os = "none", unsafe(link_section = ".rtc_noinit.ui_demo"))]
static mut SETTINGS: MaybeUninit<Settings> = MaybeUninit::uninit();

fn settings() -> Settings {
    let value = unsafe { (&raw const SETTINGS).cast::<Settings>().read_volatile() };
    if value.valid == SETTINGS_VALID {
        value
    } else {
        Settings { valid: SETTINGS_VALID, rgb888: false, bench: false, rotated: false, data_internal: false, stack_psram: false }
    }
}

fn restart_with(value: Settings) {
    unsafe { (&raw mut SETTINGS).cast::<Settings>().write_volatile(value) };
    thread::spawn(|| {
        thread::sleep(Duration::from_millis(100));
        system::restart();
    });
}

fn parse(args: &[&str]) -> Result<Params, String> {
    let kind = match args.first().copied() {
        Some("list") => Kind::List,
        Some("thumbs") => Kind::Thumbs,
        Some("dense") => Kind::Dense,
        Some("menu") => Kind::Menu,
        Some("tick") => Kind::Tick,
        Some("seek") => Kind::Seek,
        _ => return Err("bench <list|thumbs|dense|menu|tick|seek> <inplace|swap2|swap3> <rot0|rot90> <lanes> [frames] [speed] [sram|psram] [load]".to_string()),
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
    let strips_psram = args.get(6).copied() == Some("psram");
    let load = args.get(7).copied() == Some("load");
    Ok(Params { kind, present, rotated, lanes, frames, speed, strips_psram, load })
}

fn register_mode() {
    harness_rs::register("mode", |args| {
        let mut value = settings();
        match args.first().copied() {
            Some("demo") => (value.bench, value.rotated) = (false, false),
            Some("demo90") => (value.bench, value.rotated) = (false, true),
            Some("bench") => value.bench = true,
            Some("565") => value.rgb888 = false,
            Some("888") => value.rgb888 = true,
            Some("data-internal") => value.data_internal = true,
            Some("data-psram") => value.data_internal = false,
            Some("stack-psram") => value.stack_psram = true,
            Some("stack-internal") => value.stack_psram = false,
            _ => return Err("mode <demo|demo90|bench|565|888|data-internal|data-psram|stack-psram|stack-internal>".to_string()),
        }
        restart_with(value);
        Ok(None)
    });
}

pub fn run() {
    let value = settings();
    let mut config = bsp_rs::Config::default();
    config.display.fb_num = 3;
    config.display.pixel_format = Some(if value.rgb888 { PixelFormat::Rgb888 } else { PixelFormat::Rgb565 });
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
    if let Err(err) = display.set_brightness(100) {
        error!(TAG, "set_brightness: {err}");
    }
    register_mode();
    if value.bench {
        run_bench(&board, display);
    } else {
        run_demo(&board, display, &value);
    }
}

fn run_demo(board: &Board, display: &'static Display, settings: &Settings) {
    let mut config = AppConfig::new(&LIGHT);
    if settings.rotated {
        config.presenter.rotation = Rotation::R90;
    }
    if settings.data_internal {
        config.presenter.data_caps = Caps::INTERNAL;
    }
    if settings.stack_psram {
        config.presenter.lane_stack_caps = Some(Caps::SPIRAM);
    }
    config.presenter.strip_bytes = VIDEO_STRIP_BYTES;
    config.presenter.strip_caps = Caps::SPIRAM;
    let mut app = match App::new(display, board.touch, FileManager, &config) {
        Ok(app) => app,
        Err(err) => {
            error!(TAG, "app: {err}");
            return;
        }
    };
    let probe = app.probe();
    {
        let probe = probe.clone();
        harness_rs::set_idle(move || probe.idle());
    }
    let input = app.input();
    let key = {
        let input = input.clone();
        move |k| {
            input.send(k);
            Ok(None)
        }
    };
    let connect = {
        let input = input.clone();
        move |connected| {
            input.set_keyboard_connected(connected);
            Ok(None)
        }
    };
    let status = app.chrome(StatusBar::new(), Insets { top: INSET_TOP, ..Insets::default() });
    let Some(sram) = SRAM.take() else {
        error!(TAG, "strips: taken");
        return;
    };
    match app.set_strips(Strips::new(sram), 2) {
        Ok(video) => *SPARE_STRIPS.lock() = Some(video),
        Err(_) => {
            error!(TAG, "strips: refused");
            return;
        }
    }
    harness_rs::register("ui", move |args| match args {
        ["tree"] => probe.tree().map(Some),
        ["stats"] => probe.stats().map(Some),
        ["tap", id] => probe.tap(id).map(|()| None),
        ["invoke", id] => probe.invoke(id).map(|()| None),
        ["type", words @ ..] => {
            for c in words.join(" ").chars() {
                while !input.send(TextInput::Char(c)) {
                    thread::sleep(Duration::from_millis(10));
                }
            }
            Ok(None)
        }
        ["key", "backspace"] => key(TextInput::Backspace),
        ["key", "delete"] => key(TextInput::Delete),
        ["key", "enter"] => key(TextInput::Enter),
        ["key", "left"] => key(TextInput::Left),
        ["key", "right"] => key(TextInput::Right),
        ["key", "up"] => key(TextInput::Up),
        ["key", "down"] => key(TextInput::Down),
        ["key", "home"] => key(TextInput::Home),
        ["key", "end"] => key(TextInput::End),
        ["keyboard", "connected"] => connect(true),
        ["keyboard", "removed"] => connect(false),
        _ => Err("ui <tree|stats|tap <id>|invoke <id>|type <text>|key <name>|keyboard <connected|removed>>".to_string()),
    });
    let clock = thread::Builder::new().name("clock").stack_size(4096).spawn(move || {
        loop {
            thread::sleep(Duration::from_secs(1));
            status.post(StatusMsg::Tick);
        }
    });
    if let Err(err) = clock {
        error!(TAG, "clock: {err}");
    }
    if let Err(err) = harness_rs::start(board) {
        error!(TAG, "harness: {err}");
    }
    app.run();
}

fn run_bench(board: &Board, display: &'static Display) {
    let Some(sram) = SRAM.take() else {
        error!(TAG, "strips: taken");
        return;
    };
    let mut bench = match Bench::new(display, Strips::new(sram)) {
        Ok(bench) => bench,
        Err(err) => {
            error!(TAG, "bench: {err}");
            return;
        }
    };
    let running = Arc::new(AtomicBool::new(false));
    let result = Arc::new(Mutex::new(String::new()));
    let request = Arc::new(Mutex::new(None::<(bool, Params)>));
    let wake = Arc::new(Semaphore::new(0, 1));
    for (name, latency) in [("bench", false), ("bench-latency", true)] {
        let (running, request, wake) = (running.clone(), request.clone(), wake.clone());
        harness_rs::register(name, move |args| {
            if running.swap(true, Ordering::AcqRel) {
                return Err("busy".to_string());
            }
            match parse(args) {
                Ok(params) => {
                    *request.lock() = Some((latency, params));
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
    {
        let running = running.clone();
        harness_rs::set_idle(move || !running.load(Ordering::Acquire));
    }
    if let Err(err) = harness_rs::start(board) {
        error!(TAG, "harness: {err}");
    }
    info!(TAG, "bench ready {:?}", display.pixel_format());
    loop {
        wake.take();
        let Some((latency, params)) = request.lock().take() else { continue };
        let line = if latency { bench.latency(&params) } else { bench.run(&params) };
        info!(TAG, "{line}");
        *result.lock() = line;
        running.store(false, Ordering::Release);
    }
}
