/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::time::Duration;

use idf::spi::Bus;
use idf::{Result, sys, thread};

use crate::display::{ImageFormat, Rotation, Size};
use crate::driver::spi_lcd::{self, Backlight, Io, SpiLcd};

const PCLK_HZ: u32 = 40_000_000;

pub(crate) struct Config {
    pub bus: Bus,
    pub cs: i32,
    pub dc: i32,
    pub size: Size,
    pub madctl: u8,
    pub invert: bool,
    pub rotation: Rotation,
    pub backlight: Option<Backlight>,
}

/// The panel must be out of reset.
pub(crate) fn create(config: Config) -> Result<SpiLcd> {
    SpiLcd::new(spi_lcd::Config {
        bus: config.bus,
        cs: config.cs,
        dc: config.dc,
        pclk_hz: PCLK_HZ,
        size: config.size,
        pixel_format: ImageFormat::Rgb565Be,
        madctl: config.madctl,
        rotation: config.rotation,
        init: if config.invert { |io| init(io, true) } else { |io| init(io, false) },
        backlight: config.backlight,
    })
}

// SETEXTC (0xC8) unlocks the vendor commands; the ILI9341 sequence does not.
#[rustfmt::skip]
const INIT_CMDS: &[(u8, &[u8])] = &[
    (0xC8, &[0xFF, 0x93, 0x42]),
    (0xC0, &[0x12, 0x12]),
    (0xC1, &[0x03]),
    (0xC5, &[0xF2]),
    (0xB0, &[0xE0]),
    (0xF6, &[0x01, 0x00, 0x00]),
    (0xE0, &[0x00, 0x0C, 0x11, 0x04, 0x11, 0x08, 0x37, 0x89, 0x4C, 0x06, 0x0C, 0x0A, 0x2E, 0x34, 0x0F]),
    (0xE1, &[0x00, 0x0B, 0x11, 0x05, 0x13, 0x09, 0x33, 0x67, 0x48, 0x07, 0x0E, 0x0B, 0x2E, 0x33, 0x0F]),
    (0xB6, &[0x08, 0x82, 0x1D, 0x04]),
];

fn init(io: &Io, invert: bool) -> Result<()> {
    let ms = |ms| thread::sleep(Duration::from_millis(ms));
    io.tx_param(sys::LCD_CMD_SLPOUT as i32, &[])?;
    ms(120);
    for &(cmd, data) in INIT_CMDS {
        io.tx_param(i32::from(cmd), data)?;
    }
    io.tx_param(sys::LCD_CMD_COLMOD as i32, &[0x55])?;
    let invert = if invert { sys::LCD_CMD_INVON } else { sys::LCD_CMD_INVOFF };
    io.tx_param(invert as i32, &[])?;
    io.tx_param(sys::LCD_CMD_NORON as i32, &[])?;
    ms(10);
    io.tx_param(sys::LCD_CMD_DISPON as i32, &[])?;
    ms(10);
    Ok(())
}
