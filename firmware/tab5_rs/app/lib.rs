/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![no_std]

use core::ffi::CStr;
use core::time::Duration;

use bsp_rs::display::{Display, FrameBuffer, PixelFormat};
use idf::log::{error, info};
use idf::thread;

const TAG: &CStr = c"tab5_rs";

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

pub fn run() {
    let mut config = bsp_rs::Config::default();
    config.display.pixel_format = Some(PixelFormat::Rgb565);
    let board = match bsp_rs::init(&config) {
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
    let size = display.size();
    info!(
        TAG,
        "display {}x{} {:?}, {} framebuffers",
        size.width,
        size.height,
        display.pixel_format(),
        display.frame_buffer_count()
    );

    for index in 0..display.frame_buffer_count() {
        if let Some(fb) = display.frame_buffer(index) {
            draw_pattern(&fb, index);
        }
    }
    if let Err(err) = display.set_brightness(100) {
        error!(TAG, "set_brightness: {err}");
    }
    cycle(display);
}

fn cycle(display: &Display) -> ! {
    let mut index = 0;
    loop {
        if let Err(err) = display.present(index) {
            error!(TAG, "present({index}): {err}");
        }
        thread::sleep(Duration::from_secs(2));
        index = (index + 1) % display.frame_buffer_count();
    }
}

fn draw_pattern(fb: &FrameBuffer, index: usize) {
    let size = fb.size();
    let (w, h) = (usize::from(size.width), usize::from(size.height));
    for y in 0..h {
        let row = unsafe { fb.row_mut(y) };
        for x in 0..w {
            let bar = if index == 0 { x * BARS.len() / w } else { (y * BARS.len() / h + index) % BARS.len() };
            put_pixel(row, x, fb.format(), BARS[bar]);
        }
    }
}

fn put_pixel(row: &mut [u8], x: usize, format: PixelFormat, (r, g, b): (u8, u8, u8)) {
    match format {
        PixelFormat::Rgb565 => {
            let v = (u16::from(r) >> 3) << 11 | (u16::from(g) >> 2) << 5 | u16::from(b) >> 3;
            row[x * 2..x * 2 + 2].copy_from_slice(&v.to_le_bytes());
        }
        PixelFormat::Rgb888 => row[x * 3..x * 3 + 3].copy_from_slice(&[b, g, r]),
        PixelFormat::L8 => row[x] = ((u16::from(r) + u16::from(g) + u16::from(b)) / 3) as u8,
    }
}
