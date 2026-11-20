/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::time::Duration;

use idf::spi::Bus;
use idf::{Result, sys, thread};

use crate::display::{ImageFormat, Rotation, Size};
use crate::driver::spi_lcd::{self, Brightness, Io, Link, SpiLcd};

const PCLK_HZ: u32 = 80_000_000;
const ALIGNMENT: u16 = 2;

pub(crate) struct Config {
    pub bus: Bus,
    pub cs: i32,
    /// The glass, in the panel's memory.
    pub size: Size,
    pub offset: (u16, u16),
    /// What MX and MY reverse: the columns the panel drives, by the lines it
    /// scans.
    pub memory: Size,
    pub ppi: u16,
    pub rotation: Rotation,
}

/// The panel must be powered and out of reset.
pub(crate) fn create(config: Config) -> Result<SpiLcd> {
    SpiLcd::new(spi_lcd::Config {
        bus: config.bus,
        cs: config.cs,
        link: Link::Quad,
        pclk_hz: PCLK_HZ,
        size: config.size,
        memory: config.memory,
        offset: config.offset,
        alignment: ALIGNMENT,
        ppi: config.ppi,
        pixel_format: ImageFormat::Rgb565Be,
        madctl: 0,
        exchanges: false,
        rotation: config.rotation,
        init,
        brightness: Brightness::Dcs,
    })
}

#[rustfmt::skip]
const INIT_CMDS: &[(u8, &[u8])] = &[
    (0xC4, &[0x80]),
    (0x35, &[0x80]),
    (0x44, &[0x01, 0xD2]),
    (0x53, &[0x20]),
    (0x3A, &[0x55]),
];

fn init(io: &Io) -> Result<()> {
    io.tx_param(sys::LCD_CMD_SLPOUT as i32, &[])?;
    thread::sleep(Duration::from_millis(150));
    for &(cmd, data) in INIT_CMDS {
        io.tx_param(i32::from(cmd), data)?;
    }
    io.tx_param(sys::LCD_CMD_INVOFF as i32, &[])?;
    io.tx_param(sys::LCD_CMD_DISPON as i32, &[])
}
