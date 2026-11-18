/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![no_std]
// CONFIG_SOC_* flags are unknown to --check-cfg on chips without them.
#![allow(unexpected_cfgs)]

extern crate alloc;

#[cfg(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED))]
mod bench;
// Mostly the bench's, which a chip without a 2D engine leaves out.
#[cfg_attr(not(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED)), allow(dead_code))]
mod scene;
mod screens;

mod resources {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/resources.rs"));
}

use alloc::boxed::Box;
use alloc::string::ToString;
use core::ffi::CStr;
use core::mem::MaybeUninit;
use core::sync::atomic::Ordering;
use core::time::Duration;

use bsp_rs::Board;
use bsp_rs::display::{Display, ImageFormat};
use devkit_core::{BufferBytes, BufferPool};
use devkit_ui::{App, AppConfig, AutoRotation, ColorMode, Rotation, SavedColorMode, StripSource, Strips, TextInput};
use idf::heap::Caps;
use idf::log::error;
use idf::sync::FreeRtos;
use idf::{system, thread};

use screens::{Home, StatusBar, StatusMsg};

const TAG: &CStr = c"ui_demo";
const SETTINGS_VALID: u32 = 0x5549_4445;
#[cfg(CONFIG_BSP_RS_BOARD_TAB5)]
const STRIP_BYTES: usize = 4 * 64 * 1024;
#[cfg(not(CONFIG_BSP_RS_BOARD_TAB5))]
const STRIP_BYTES: usize = 4 * 320 * 16 * 2;

static SRAM: BufferPool<BufferBytes<STRIP_BYTES>, 1, FreeRtos> = BufferPool::new(BufferBytes::new());

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
    if cfg!(CONFIG_BSP_RS_BOARD_TAB5) {
        config.display.fb_num = 3;
        config.display.pixel_format = Some(if value.rgb888 { ImageFormat::Bgr888 } else { ImageFormat::Rgb565 });
    }
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
    screens::set_display(display);
    let theme = if cfg!(CONFIG_BSP_RS_BOARD_TAB5) { &resources::TOUCH } else { &resources::SMALL };
    let mut config = AppConfig::new(theme);
    config.plugins.push(Box::new(SavedColorMode::new()));
    if settings.rotated {
        config.presenter.rotation = Rotation::R90;
    }
    if !settings.rotated {
        let rotation = AutoRotation::new(board.imu);
        config.plugins.push(Box::new(rotation.clone()));
        screens::set_rotation(rotation);
    }
    if settings.data_internal {
        config.presenter.data_caps = Caps::INTERNAL.union(Caps::ACCESS_8BIT);
    }
    if settings.stack_psram {
        config.presenter.lane_stack_caps = Caps::SPIRAM;
    }
    let Some(sram) = SRAM.alloc(STRIP_BYTES, 64) else {
        error!(TAG, "strips: taken");
        return;
    };
    config.presenter.strips = StripSource::Given(Strips::new(sram));
    let mut app = match App::new(display, board.touch, Home, config) {
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
    screens::set_input(input.clone());
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
    let status = app.chrome(StatusBar::new());
    harness_rs::register("video", |_| Ok(Some(screens::VIDEO_FRAMES.load(Ordering::Relaxed).to_string())));
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
        ["dark", "on"] => input.set_color_mode(ColorMode::Dark).then_some(None).ok_or_else(|| "queue".to_string()),
        ["dark", "off"] => input.set_color_mode(ColorMode::Light).then_some(None).ok_or_else(|| "queue".to_string()),
        ["card", "out"] => screens::set_card_present(false).then_some(None).ok_or_else(|| "queue".to_string()),
        ["card", "in"] => screens::set_card_present(true).then_some(None).ok_or_else(|| "queue".to_string()),
        _ => Err("ui <tree|stats|tap <id>|invoke <id>|type <text>|key <name>|keyboard <connected|removed>|card <in|out>>".to_string()),
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
    #[cfg(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED))]
    {
        let Some(sram) = SRAM.alloc(STRIP_BYTES, 64) else {
            error!(TAG, "strips: taken");
            return;
        };
        bench::serve(board, display, Strips::new(sram));
    }
    #[cfg(not(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED)))]
    {
        let _ = (board, display);
        error!(TAG, "bench: needs a 2D engine");
    }
}
