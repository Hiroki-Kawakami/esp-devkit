/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![no_std]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use core::ffi::CStr;
use core::time::Duration;

use bsp_rs::display::{Display, Image, ImageFormat, Rect, Rotation};
use bsp_rs::touch::{MAX_POINTS, Point, Touch};
use devkit_gfx::swap_rgb565;
use idf::heap::Caps;
use idf::log::{error, info};
use idf::thread;
use idf::time::Instant;

const TAG: &CStr = c"stopwatch_rs";

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
const MARK_TOP: i16 = 16;
const EDGE: i16 = 2;
const EDGE_SPAN: i16 = 64;
const HOLD: Duration = Duration::from_secs(3);
const FRAME_HOLD: Duration = Duration::from_millis(1500);
const DOT: i16 = 8;
const TOUCH_POLL: Duration = Duration::from_millis(10);
const RED: (u8, u8, u8) = (255, 0, 0);
const GREEN: (u8, u8, u8) = (0, 255, 0);
const BLUE: (u8, u8, u8) = (0, 0, 255);
const GRAY: (u8, u8, u8) = (128, 128, 128);
const ORANGE: (u8, u8, u8) = (255, 128, 0);

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
    for rotation in ROTATIONS.iter().cycle() {
        let mut config = bsp_rs::display::Config::default();
        config.rotation = Some(*rotation);
        if let Err(err) = unsafe { display.reconfigure(&config) } {
            info!(TAG, "rotation {rotation:?}: {err}");
            continue;
        }
        if let Some(touch) = board.touch {
            touch.set_rotation(*rotation);
        }
        let size = display.size();
        info!(TAG, "{:?}: {}x{} {:?} align {}", display.rotation(), size.width, size.height, display.pixel_format(), display.alignment());
        full_frame_from_psram(display);
        thread::sleep(FRAME_HOLD);
        draw_bars(display);
        mark_top(display);
        mark_edges(display);
        match board.touch {
            Some(touch) => follow_touch(display, touch),
            None => thread::sleep(HOLD),
        }
    }
}

// A dot under each finger, for `HOLD`. Logs where a touch starts and ends, so
// a drag to the rim shows the reach of the coordinates.
fn follow_touch(display: &Display, touch: &Touch) {
    let size = display.size();
    let dot = pixels(display, DOT, DOT, (0, 0, 0));
    let mut points = [Point::default(); MAX_POINTS];
    let mut last = 0;
    let mut latest = Point::default();
    let start = Instant::now();
    while start.elapsed() < HOLD {
        let n = touch.read(&mut points);
        for p in &points[..n] {
            let x = snap(display, (p.x as i16 - DOT / 2).clamp(0, size.width as i16 - DOT));
            let y = snap(display, (p.y as i16 - DOT / 2).clamp(0, size.height as i16 - DOT));
            let written = unsafe { display.write(Rect::new(x, y, DOT, DOT), &dot) }.and_then(|t| display.wait(t));
            if let Err(err) = written {
                error!(TAG, "dot at {},{}: {err}", p.x, p.y);
            }
        }
        if n != last {
            match points[..n].first() {
                Some(p) => info!(TAG, "touch: {n} at {},{}", p.x, p.y),
                None => info!(TAG, "touch: released at {},{}", latest.x, latest.y),
            }
            last = n;
        }
        if let Some(p) = points[..n].first() {
            latest = *p;
        }
        thread::sleep(TOUCH_POLL);
    }
}

// The whole panel in one write from PSRAM, timed: a red-to-blue sweep down
// and green across, so a stalled transfer shows as a broken band.
fn full_frame_from_psram(display: &Display) {
    let size = display.size();
    let (w, h) = (usize::from(size.width), usize::from(size.height));
    let mut pixels = Vec::new_in(Caps::SPIRAM);
    if pixels.try_reserve_exact(w * h * 2).is_err() {
        error!(TAG, "full frame: no PSRAM for {} bytes", w * h * 2);
        return;
    }
    for y in 0..h {
        for x in 0..w {
            let (r, g, b) = (255 - y * 255 / h, x * 255 / w, y * 255 / h);
            let v = (r as u16 >> 3) << 11 | (g as u16 >> 2) << 5 | b as u16 >> 3;
            pixels.extend_from_slice(&v.to_be_bytes());
        }
    }
    if display.pixel_format() != ImageFormat::Rgb565Be {
        swap_rgb565(&mut pixels);
    }
    let start = Instant::now();
    let written = unsafe { display.write(Rect::new(0, 0, w as i16, h as i16), &pixels) }.and_then(|t| display.wait(t));
    match written {
        Ok(()) => info!(TAG, "full frame from PSRAM: {} us for {} bytes", start.elapsed().as_micros(), pixels.len()),
        Err(err) => error!(TAG, "full frame: {err}"),
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

fn fill(display: &Display, rect: Rect, color: (u8, u8, u8)) {
    let pixels = pixels(display, rect.w, rect.h, color);
    let written = unsafe { display.write(rect, &pixels) }.and_then(|t| display.wait(t));
    if let Err(err) = written {
        error!(TAG, "fill {rect:?}: {err}");
    }
}

fn draw_bars(display: &Display) {
    let size = display.size();
    let (w, h) = (size.width as i16, size.height as i16);
    let n = BARS.len() as i16;
    let edge = |i: i16| snap(display, w * i / n);
    for (i, color) in BARS.iter().enumerate() {
        let (x0, x1) = (edge(i as i16), edge(i as i16 + 1));
        let pixels = pixels(display, x1 - x0, h, *color);
        let image = Image::from_pixels(display.pixel_format(), (x1 - x0) as u16, h as u16, (x1 - x0) as usize, &pixels);
        if let Err(err) = unsafe { display.draw(Rect::new(x0, 0, x1 - x0, h), &image) } {
            error!(TAG, "draw bar {i}: {err}");
        }
    }
}

// Red near the top middle of the logical picture, which a round panel shows.
fn mark_top(display: &Display) {
    let w = display.size().width as i16;
    fill(display, Rect::new(snap(display, w / 2 - MARK / 2), MARK_TOP, MARK, MARK), RED);
}

fn snap(display: &Display, v: i16) -> i16 {
    let align = display.alignment() as i16;
    v / align * align
}

// Lines on the outermost columns and rows, where the round glass meets them:
// all four show only when the picture sits on the glass.
fn mark_edges(display: &Display) {
    let size = display.size();
    let (w, h) = (size.width as i16, size.height as i16);
    let (mid_x, mid_y) = (snap(display, w / 2 - EDGE_SPAN / 2), snap(display, h / 2 - EDGE_SPAN / 2));
    fill(display, Rect::new(0, mid_y, EDGE, EDGE_SPAN), GREEN);
    fill(display, Rect::new(w - EDGE, mid_y, EDGE, EDGE_SPAN), ORANGE);
    fill(display, Rect::new(mid_x, 0, EDGE_SPAN, EDGE), BLUE);
    fill(display, Rect::new(mid_x, h - EDGE, EDGE_SPAN, EDGE), GRAY);
}
