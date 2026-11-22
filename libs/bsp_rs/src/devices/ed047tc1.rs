/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! The ED047TC1 4.7" 960x540 EPD: its timing and waveforms on `epd_ll`.

use alloc::boxed::Box;

use idf::Result;

use crate::display::{ImageFormat, Size};
use crate::driver::epd_ll::{self, Control, EpdLl, Waveforms};
use crate::driver::epd_ll::waveform::{B, Frame, SETTLE, STOP, W, by_dc as d, by_to as t, check};

pub(crate) const SIZE: Size = Size { width: 960, height: 540 };
const PCLK_HZ: u32 = 20_000_000;
const LINE_BYTES: u16 = SIZE.width / 4;
const LINE_PADDING: u8 = 16;

pub(crate) struct Config {
    pub data: [i32; 8],
    pub sph: i32,
    pub cl: i32,
    pub dc_dummy: i32,
    pub control: Box<dyn Control>,
    pub format: ImageFormat,
    pub ppi: u16,
    pub priority: u32,
    pub core: Option<u32>,
}

// Takes the DC a replay starts from back toward zero, a frame per unit. The
// frames after it leave at most 4 either way, so 4 frames cancel it all.
#[rustfmt::skip]
const CANCEL: [Frame; 4] = [
    //-8 -7 -6 -5 -4 -3 -2 -1  0  1  2  3  4  5  6  7
    d([B, B, B, B, B, B, B, B, 0, W, W, W, W, W, W, W]),
    d([B, B, B, B, B, B, B, 0, 0, 0, W, W, W, W, W, W]),
    d([B, B, B, B, B, B, 0, 0, 0, 0, 0, W, W, W, W, W]),
    d([B, B, B, B, B, 0, 0, 0, 0, 0, 0, 0, W, W, W, W]),
];

// 16 grays with flashing, the cleanest.
#[rustfmt::skip]
static QUALITY: [Frame; 34] = [
    CANCEL[0], CANCEL[1], CANCEL[2], CANCEL[3],
    // 0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15
    t([B, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B]),
    t([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W]),
    t([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W]),
    t([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W]),
    t([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W]),
    t([B, B, B, B, B, B, B, W, 0, W, B, B, B, W, W, B]),
    t([W, W, W, W, W, B, B, B, B, B, W, B, 0, B, B, B]),
    t([B, B, B, B, B, B, B, B, W, B, B, B, B, B, B, B]),
    t([B, W, B, B, B, W, W, B, B, B, B, W, W, B, B, B]),
    t([B, B, W, 0, B, B, 0, B, B, B, B, B, B, B, B, W]),
    t([B, B, 0, W, W, W, 0, 0, B, B, B, B, B, B, 0, W]),
    t([B, B, B, B, B, B, B, B, B, 0, 0, 0, 0, W, W, W]),
    t([B, B, B, B, 0, 0, W, W, W, W, W, W, W, W, W, W]),
    t([B, B, B, 0, 0, 0, B, 0, 0, 0, 0, 0, 0, 0, 0, W]),
    SETTLE, SETTLE, SETTLE, SETTLE, SETTLE, SETTLE, SETTLE, SETTLE,
    SETTLE, SETTLE, SETTLE, SETTLE, SETTLE, SETTLE, SETTLE, SETTLE,
];

#[rustfmt::skip]
static TEXT: [Frame; 20] = [
    CANCEL[0], CANCEL[1], CANCEL[2], CANCEL[3],
    // 0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15
    t([W, W, W, W, W, W, W, W, B, B, B, B, B, B, B, B]),
    t([W, W, W, W, W, W, W, W, B, B, B, B, B, B, B, B]),
    t([B, W, W, W, W, W, W, W, B, B, B, B, B, B, B, W]),
    t([B, B, 0, W, W, W, W, W, B, B, B, B, 0, 0, W, W]),
    t([B, B, B, B, B, B, B, B, W, W, W, W, W, W, W, W]),
    t([B, B, B, B, B, B, B, B, W, W, W, W, W, W, W, W]),
    t([B, B, B, B, B, B, B, B, W, W, W, W, W, W, W, W]),
    t([B, B, B, B, B, B, B, B, W, W, W, W, W, W, W, W]),
    SETTLE, SETTLE, SETTLE, SETTLE, SETTLE, SETTLE, SETTLE, SETTLE,
];

// Two levels: only black and white targets drive, grays hold.
#[rustfmt::skip]
static FAST: [Frame; 16] = [
    CANCEL[0], CANCEL[1], CANCEL[2], CANCEL[3],
    // 0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15
    t([W, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, B]),
    t([W, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, B]),
    t([B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W]),
    t([B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W]),
    t([B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W]),
    t([B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W]),
    t([B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W]),
    t([B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W]),
    SETTLE, SETTLE, SETTLE, SETTLE,
];

// Its frames drive every pixel alike, so one line serves every row. The tail
// is STOP: cut short, the next refresh would start from unsettled glass.
#[rustfmt::skip]
static CLEAR: [Frame; 28] = [
    CANCEL[0], CANCEL[1], CANCEL[2], CANCEL[3],
    // 0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15
    t([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W]),
    t([B, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B]),
    t([B, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B]),
    t([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W]),
    t([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W]),
    t([B, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B]),
    t([B, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B]),
    t([B, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B]),
    t([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W]),
    t([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W]),
    t([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W]),
    t([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W]),
    t([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W]),
    t([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W]),
    STOP, STOP, STOP, STOP, STOP, STOP, STOP, STOP,
    STOP, STOP,
];

const _: () = assert!(check(&QUALITY));
const _: () = assert!(check(&TEXT));
const _: () = assert!(check(&FAST));
const _: () = assert!(check(&CLEAR));

pub(crate) fn create(config: Config) -> Result<EpdLl> {
    EpdLl::new(epd_ll::Config {
        data: config.data,
        sph: config.sph,
        cl: config.cl,
        dc_dummy: config.dc_dummy,
        control: config.control,
        pclk_hz: PCLK_HZ,
        size: SIZE,
        format: config.format,
        ppi: config.ppi,
        line_bytes: LINE_BYTES,
        line_padding: LINE_PADDING,
        waveforms: Waveforms { fast: Some(&FAST), quality: Some(&QUALITY), text: Some(&TEXT), clear: &CLEAR },
        priority: config.priority,
        core: config.core,
    })
}
