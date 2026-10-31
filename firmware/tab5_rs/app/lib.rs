/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![no_std]

extern crate alloc;

use core::ffi::CStr;
use core::time::Duration;

use alloc::boxed::Box;
use alloc::vec::Vec;

use bsp_rs::audio::{Audio, Format, Pcm, SampleFormat, SpeakerMode};
use bsp_rs::display::{Display, FrameBuffer, PixelFormat};
use devkit::math::sinf;
use bsp_rs::touch::{Point, Touch};
use idf::log::{error, info};
use idf::thread;

const TAG: &CStr = c"tab5_rs";

const TONE_RATE: u32 = 48_000;
const TONE_HZ: f32 = 440.0;
const TONE_SECONDS: usize = 2;
const TONE_VOLUME: u8 = 70;
const TONE_CHUNK: usize = 480;
const TONE_REPEATS: usize = 2;
const TONE_GAP: Duration = Duration::from_secs(1);

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
    config.audio.speaker_mode = SpeakerMode::Auto;
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
    if let Some(touch) = board.touch {
        trace_touches(display, touch);
    }
    if let Err(err) = harness_rs::start(&board) {
        error!(TAG, "harness: {err}");
    }
    if let Some(audio) = board.audio {
        play_tone(audio);
    }
    match board.touch {
        Some(touch) => log_touches(touch),
        None => cycle(display),
    }
}

fn play_tone(audio: &'static Audio) {
    match audio.set_headphone_callback(Some(Box::new(|inserted| info!(TAG, "headphone: {inserted}")))) {
        Err(err) if err != idf::Error::NOT_SUPPORTED => error!(TAG, "set_headphone_callback: {err}"),
        _ => {}
    }
    if let Err(err) = audio.set_volume(TONE_VOLUME) {
        error!(TAG, "set_volume: {err}");
    }
    let spawned = thread::Builder::new().name("tone").spawn(move || {
        let format = Format { sample_rate: TONE_RATE, sample: SampleFormat::S16, channels: 2 };
        for _ in 0..TONE_REPEATS {
            if let Err(err) = tone(audio, format) {
                error!(TAG, "tone: {err}");
            }
            thread::sleep(TONE_GAP);
        }
    });
    if let Err(err) = spawned {
        error!(TAG, "tone thread: {err}");
    }
}

fn tone(audio: &Audio, format: Format) -> idf::Result<()> {
    let mut stream = audio.open(format)?;
    let step = core::f32::consts::TAU * TONE_HZ / TONE_RATE as f32;
    let mut phase = 0.0f32;
    let mut pcm = Vec::with_capacity(TONE_CHUNK * 2);
    for _ in 0..TONE_SECONDS * TONE_RATE as usize / TONE_CHUNK {
        pcm.clear();
        for _ in 0..TONE_CHUNK {
            let sample = (sinf(phase) * f32::from(i16::MAX / 2)) as i16;
            pcm.extend_from_slice(&[sample, sample]);
            phase = (phase + step) % core::f32::consts::TAU;
        }
        stream.write(Pcm::S16(&pcm))?;
    }
    info!(TAG, "tone queued {:?} frames", stream.queued());
    stream.close()
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

fn trace_touches(display: &'static Display, touch: &Touch) {
    if let Err(err) = display.present(0) {
        error!(TAG, "present(0): {err}");
    }
    touch.set_event_cb(Some(Box::new(move |points: &[Point]| {
        let Some(fb) = display.frame_buffer(0) else { return };
        for p in points {
            fill_square(&fb, *p, 16, BARS[0]);
        }
        if !points.is_empty() && let Err(err) = display.present(0) {
            error!(TAG, "present(0): {err}");
        }
    })));
}

fn log_touches(touch: &Touch) -> ! {
    let mut last = 0;
    let mut points = [Point::default(); bsp_rs::touch::MAX_POINTS];
    loop {
        touch.wait_interrupt();
        let n = touch.read(&mut points);
        if n != last {
            match n {
                0 => info!(TAG, "released"),
                _ => info!(TAG, "{n} touch(es), first at {},{} id {}", points[0].x, points[0].y, points[0].id),
            }
            last = n;
        }
    }
}

fn fill_square(fb: &FrameBuffer, center: Point, half: u16, color: (u8, u8, u8)) {
    let size = fb.size();
    let x0 = center.x.saturating_sub(half);
    let x1 = (center.x + half).min(size.width);
    for y in center.y.saturating_sub(half)..(center.y + half).min(size.height) {
        let row = unsafe { fb.row_mut(usize::from(y)) };
        for x in x0..x1 {
            put_pixel(row, usize::from(x), fb.format(), color);
        }
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
