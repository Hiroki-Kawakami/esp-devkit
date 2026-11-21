/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! Waveform tuning for EPD panels, driven from `tool/index.html` over the
//! console.

#![no_std]

extern crate alloc;

mod picture;
mod serial;

mod resources {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/resources.rs"));
}

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::CStr;

use bsp_rs::display::{Display, Epd, EpdMode, Image, ImageFormat, Rect, Waveform};
use idf::log::error;
use idf::sync::{Sender, channel};

use picture::Picture;

const TAG: &CStr = c"epd_tweak";
const JOB_DEPTH: usize = 4;

pub(crate) enum Job {
    Pattern,
    /// Answers the size the image took.
    Image(Vec<u8>, Sender<Result<(u16, u16), String>>),
    /// Clears the glass and shows the picture again, from white.
    Clean,
}

pub fn run() {
    let board = match bsp_rs::init(&bsp_rs::Config::default()) {
        Ok(board) => board,
        Err(err) => {
            error!(TAG, "bsp_rs::init failed: {err}");
            return;
        }
    };
    let Some(display) = board.display else {
        error!(TAG, "no display");
        return;
    };
    let Some(epd) = display.epd() else {
        error!(TAG, "the display is not an EPD");
        return;
    };
    if let Err(err) = epd.set_mode(EpdMode::Hold) {
        error!(TAG, "set_mode: {err}");
    }
    let (jobs, queue) = channel(JOB_DEPTH);
    if let Some(touch) = board.touch {
        let tap = jobs.clone();
        let mut down = false;
        touch.set_event_cb(Some(Box::new(move |points| {
            if !points.is_empty() && !down {
                let _ = tap.try_send(Job::Clean);
            }
            down = !points.is_empty();
        })));
    }
    serial::register(display, jobs.clone());
    if let Err(err) = harness_rs::start(&board) {
        error!(TAG, "harness: {err}");
    }
    jobs.send(Job::Pattern);

    let mut shown: Option<Picture> = None;
    loop {
        match queue.recv() {
            Job::Pattern => match picture::pattern(display.size()) {
                Ok(picture) => shown = Some(show(display, epd, picture)),
                Err(err) => error!(TAG, "pattern: {err}"),
            },
            Job::Image(data, done) => {
                let result = picture::image(display.size(), &data);
                drop(data);
                done.send(result.map(|(picture, size)| {
                    shown = Some(show(display, epd, picture));
                    size
                }));
            }
            Job::Clean => {
                if let Err(err) = epd.clear() {
                    error!(TAG, "clear: {err}");
                }
                if let Some(picture) = shown.take() {
                    shown = Some(show(display, epd, picture));
                }
            }
        }
    }
}

/// Draws `picture` into the panel's memory, then drives every pixel of it.
fn show(display: &Display, epd: Epd<'_>, picture: Picture) -> Picture {
    let size = picture.size;
    let area = Rect::new(0, 0, size.width as i16, size.height as i16);
    let image = Image::from_pixels(ImageFormat::L8, size.width, size.height, usize::from(size.width), &picture.pixels);
    // Only this task draws.
    if let Err(err) = unsafe { display.draw(area, &image) } {
        error!(TAG, "draw: {err}");
    } else if let Err(err) = epd.refresh(area, Waveform::Quality, true) {
        error!(TAG, "refresh: {err}");
    }
    picture
}
