/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::f32::consts::TAU;
use core::ffi::CStr;
use core::time::Duration;

use bsp_rs::audio::{Audio, Format, Pcm, SampleFormat};
use bsp_rs::display::{Display, Image, ImageFormat, Rect, Rotation};
use bsp_rs::touch::{MAX_POINTS, Point, Touch};
use devkit_core::math::sinf;
use devkit_gfx::swap_rgb565;
use idf::log::{error, info};
use idf::thread;
use idf::time::Instant;

const TAG: &CStr = c"core2_rs";

const BARS: [(u8, u8, u8); 8] = [
    (255, 255, 255),
    (255, 255, 0),
    (0, 255, 255),
    (0, 255, 0),
    (255, 0, 255),
    (255, 0, 0),
    (0, 0, 255),
    (0, 0, 0),
];
const ROTATIONS: [Rotation; 4] = [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270];
const MARK: i16 = 32;
const HOLD: Duration = Duration::from_secs(3);
const DOT: i16 = 8;
const TOUCH_POLL: Duration = Duration::from_millis(10);
const BEEP_RATE: u32 = 48_000;
const BEEP_HZ: f32 = 880.0;
const BEEP_LENGTH: Duration = Duration::from_millis(150);
const BEEP_GAP: Duration = Duration::from_millis(500);
const BEEP_CHUNK: usize = 480;
const BEEP_VOLUME: u8 = 70;
const DOUBLE_CLICK: Duration = Duration::from_millis(300);
const LONG_PRESS: Duration = Duration::from_millis(800);

pub fn run() {
    let board = match bsp_rs::init(&bsp_rs::Config::default()) {
        Ok(board) => board,
        Err(err) => {
            error!(TAG, "bsp_rs::init failed: {err}");
            return;
        }
    };
    let Some(display) = board.display else {
        info!(TAG, "no display");
        return;
    };
    if let Err(err) = display.set_brightness(100) {
        error!(TAG, "set_brightness: {err}");
    }
    if let Some(buttons) = board.buttons {
        buttons.set_double_click(1, Some(DOUBLE_CLICK));
        buttons.set_long_press(2, Some(LONG_PRESS));
        buttons.set_event_cb(Some(Box::new(|id, event| info!(TAG, "button {id}: {event:?}"))));
    }
    if let Some(audio) = board.audio
        && let Err(err) = audio.set_volume(BEEP_VOLUME)
    {
        error!(TAG, "set_volume: {err}");
    }
    for rotation in ROTATIONS.iter().cycle() {
        if let Err(err) = unsafe { display.reconfigure(None, Some(*rotation)) } {
            error!(TAG, "rotation {rotation:?}: {err}");
        }
        if let Some(touch) = board.touch {
            touch.set_rotation(*rotation);
        }
        let size = display.size();
        info!(TAG, "{:?}: {}x{} {:?}", display.rotation(), size.width, size.height, display.pixel_format());
        match board.power.read() {
            Ok(state) => info!(TAG, "power: {state:?}"),
            Err(err) => error!(TAG, "power: {err}"),
        }
        if let Some(rtc) = board.rtc {
            match rtc.is_valid().and_then(|valid| Ok((valid, rtc.now()?))) {
                Ok((valid, now)) => info!(TAG, "rtc: {now:?} valid={valid}"),
                Err(err) => error!(TAG, "rtc: {err}"),
            }
        }
        draw_bars(display);
        mark_top(display);
        if let Some(audio) = board.audio {
            beep_twice(audio);
        }
        match board.touch {
            Some(touch) => follow_touch(display, touch),
            None => thread::sleep(HOLD),
        }
    }
}

// The first beep powers the amp up; the second follows within its close delay,
// so the two heads show whether powering up cuts into a stream.
fn beep_twice(audio: &Audio) {
    for i in 0..2 {
        if i > 0 {
            thread::sleep(BEEP_GAP);
        }
        if let Err(err) = beep(audio) {
            error!(TAG, "beep: {err}");
        }
    }
}

fn beep(audio: &Audio) -> idf::Result<()> {
    let format = Format { sample_rate: BEEP_RATE, sample: SampleFormat::S16, channels: 2 };
    let mut stream = audio.open(format)?;
    let step = TAU * BEEP_HZ / BEEP_RATE as f32;
    let mut phase = 0.0f32;
    let mut pcm = Vec::with_capacity(BEEP_CHUNK * 2);
    for _ in 0..BEEP_LENGTH.as_millis() as usize * BEEP_RATE as usize / 1000 / BEEP_CHUNK {
        pcm.clear();
        for _ in 0..BEEP_CHUNK {
            let sample = (sinf(phase) * f32::from(i16::MAX / 2)) as i16;
            pcm.extend_from_slice(&[sample, sample]);
            phase = (phase + step) % TAU;
        }
        stream.write(Pcm::S16(&pcm))?;
    }
    stream.close()
}

// A dot under each finger, for `HOLD`.
fn follow_touch(display: &Display, touch: &Touch) {
    let Some(gram) = display.gram() else {
        error!(TAG, "follow_touch: no gram");
        return;
    };
    let size = display.size();
    let dot = pixels(display, DOT, DOT, (0, 0, 0));
    let mut points = [Point::default(); MAX_POINTS];
    let mut last = 0;
    let start = Instant::now();
    while start.elapsed() < HOLD {
        let n = touch.read(&mut points);
        for p in &points[..n] {
            let x = (p.x as i16 - DOT / 2).clamp(0, size.width as i16 - DOT);
            let y = (p.y as i16 - DOT / 2).clamp(0, size.height as i16 - DOT);
            let written = unsafe { gram.write(Rect::new(x, y, DOT, DOT), &dot) }.and_then(|t| gram.wait(t));
            if let Err(err) = written {
                error!(TAG, "dot at {},{}: {err}", p.x, p.y);
            }
        }
        if n != last {
            match points[..n].first() {
                Some(p) => info!(TAG, "touch: {n} at {},{}", p.x, p.y),
                None => info!(TAG, "touch: released"),
            }
            last = n;
        }
        thread::sleep(TOUCH_POLL);
    }
}

fn pixels(display: &Display, w: i16, h: i16, (r, g, b): (u8, u8, u8)) -> Vec<u8> {
    let v = (u16::from(r) >> 3) << 11 | (u16::from(g) >> 2) << 5 | u16::from(b) >> 3;
    let mut pixels: Vec<u8> = vec![0; w as usize * h as usize * 2];
    for p in pixels.chunks_exact_mut(2) {
        p.copy_from_slice(&v.to_le_bytes());
    }
    if display.pixel_format() == ImageFormat::Rgb565Be {
        swap_rgb565(&mut pixels);
    }
    pixels
}

fn draw_bars(display: &Display) {
    let size = display.size();
    let (w, h) = (size.width as i16, size.height as i16);
    let n = BARS.len() as i16;
    for (i, color) in BARS.iter().enumerate() {
        let (x0, x1) = (w * i as i16 / n, w * (i as i16 + 1) / n);
        let pixels = pixels(display, x1 - x0, h, *color);
        let image = Image::from_pixels(display.pixel_format(), (x1 - x0) as u16, h as u16, (x1 - x0) as usize, &pixels);
        if let Err(err) = unsafe { display.draw(Rect::new(x0, 0, x1 - x0, h), &image) } {
            error!(TAG, "draw bar {i}: {err}");
        }
    }
}

// Red at the top left and blue at the top right of the logical picture.
fn mark_top(display: &Display) {
    let Some(gram) = display.gram() else {
        error!(TAG, "mark_top: no gram");
        return;
    };
    let w = display.size().width as i16;
    for (x, color) in [(0, (255, 0, 0)), (w - MARK, (0, 0, 255))] {
        let pixels = pixels(display, MARK, MARK, color);
        let written = unsafe { gram.write(Rect::new(x, 0, MARK, MARK), &pixels) }.and_then(|t| gram.wait(t));
        if let Err(err) = written {
            error!(TAG, "mark at {x}: {err}");
        }
    }
}
