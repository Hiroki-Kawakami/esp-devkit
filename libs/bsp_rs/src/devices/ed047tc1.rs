/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! The ED047TC1 4.7" 960x540 EPD: its timing and waveforms on `epd_ll`.

use alloc::boxed::Box;

use idf::Result;

use crate::display::Size;
use crate::driver::epd_ll::{self, Control, EpdLl, Waveforms};
use crate::driver::epd_waveform::{B, Frame, SETTLE, STOP, W, Waveform, by_from as f, by_to as t};

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
    pub ppi: u16,
    pub priority: u32,
    pub core: Option<u32>,
}

// 16 grays with flashing, the cleanest. The flashes go by the gray on the
// glass, so pixels already white flash against the rest.
#[rustfmt::skip]
static QUALITY: [Frame; 34] = [
    //   0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15
    f([W, W, W, W, W, W, W, W, B, B, B, B, B, B, B, B]),
    f([W, W, 0, B, B, B, B, B, W, W, W, W, 0, 0, B, B]),
    f([W, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B]),
    f([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, B]),
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
    //   0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15
    f([W, B, B, B, B, B, B, B, W, W, W, W, W, W, W, B]),
    f([W, W, 0, B, B, B, B, B, W, W, W, W, 0, 0, B, B]),
    f([W, W, W, W, W, W, W, W, B, B, B, B, B, B, B, B]),
    f([W, W, W, W, W, W, W, W, B, B, B, B, B, B, B, B]),
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
static FAST: [Frame; 12] = [
    //   0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15
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
    //   0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15
    f([W, W, W, W, W, W, W, W, B, B, B, B, B, B, B, B]),
    f([W, W, 0, B, B, B, B, B, W, W, W, W, 0, 0, B, B]),
    f([W, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B]),
    f([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, B]),
    t([B, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B]),
    t([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W]),
    t([B, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B]),
    t([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W]),
    t([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W]),
    t([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W]),
    t([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W]),
    t([W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W]),
    STOP, STOP, STOP, STOP, STOP, STOP, STOP, STOP,
    STOP, STOP, STOP, STOP, STOP, STOP, STOP, STOP,
];

const _: () = assert!(Waveform::new(&QUALITY).is_some());
const _: () = assert!(Waveform::new(&TEXT).is_some());
const _: () = assert!(Waveform::new(&FAST).is_some());
const _: () = assert!(Waveform::new(&CLEAR).is_some());

pub(crate) fn create(config: Config) -> Result<EpdLl> {
    EpdLl::new(epd_ll::Config {
        data: config.data,
        sph: config.sph,
        cl: config.cl,
        dc_dummy: config.dc_dummy,
        control: config.control,
        pclk_hz: PCLK_HZ,
        size: SIZE,
        ppi: config.ppi,
        line_bytes: LINE_BYTES,
        line_padding: LINE_PADDING,
        waveforms: Waveforms { fast: Some(&FAST), quality: Some(&QUALITY), text: Some(&TEXT), clear: &CLEAR },
        priority: config.priority,
        core: config.core,
    })
}
