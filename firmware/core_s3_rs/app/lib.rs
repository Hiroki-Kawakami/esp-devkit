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
use devkit_gfx::swap_rgb565;
use idf::log::{error, info};
use idf::thread;

const TAG: &CStr = c"core_s3_rs";

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
            error!(TAG, "rotation {rotation:?}: {err}");
        }
        let size = display.size();
        info!(TAG, "{:?}: {}x{} {:?}", display.rotation(), size.width, size.height, display.pixel_format());
        draw_bars(display);
        mark_top(display);
        thread::sleep(HOLD);
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
    let w = display.size().width as i16;
    for (x, color) in [(0, (255, 0, 0)), (w - MARK, (0, 0, 255))] {
        let pixels = pixels(display, MARK, MARK, color);
        let written = unsafe { display.write(Rect::new(x, 0, MARK, MARK), &pixels) }.and_then(|t| display.wait(t));
        if let Err(err) = written {
            error!(TAG, "mark at {x}: {err}");
        }
    }
}
