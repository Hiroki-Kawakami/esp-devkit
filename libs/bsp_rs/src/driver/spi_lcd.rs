/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::ffi::CStr;

use idf::lcd::spi::{Io, IoConfig, IoGuard};
use idf::log::error;
use idf::spi::Bus;
use idf::{Error, Result, sys};

use crate::display::{Driver, Image, ImageFormat, Rect, Rotation, Size, Ticket};

const TAG: &CStr = c"spi_lcd";

const QUEUE_DEPTH: usize = 10;
const MADCTL_MY: u8 = 0x80;
const MADCTL_MX: u8 = 0x40;
const MADCTL_MV: u8 = 0x20;

pub(crate) type Backlight = Box<dyn Fn(u8) -> Result<()> + Send + Sync>;

pub(crate) struct Config {
    pub bus: Bus,
    pub cs: i32,
    pub dc: i32,
    pub pclk_hz: u32,
    /// Unturned.
    pub size: Size,
    pub pixel_format: ImageFormat,
    /// For the unturned picture; a rotation flips its MV, MX and MY bits.
    pub madctl: u8,
    pub rotation: Rotation,
    /// Everything up to the picture showing, but MADCTL.
    pub init: fn(&IoGuard) -> Result<()>,
    /// 0..=100.
    pub backlight: Option<Backlight>,
}

/// A panel that keeps the picture in its own memory, written with the MIPI DCS
/// commands over SPI.
pub(crate) struct SpiLcd {
    io: Io,
    size: Size,
    format: ImageFormat,
    madctl: u8,
    rotation: Rotation,
    backlight: Option<Backlight>,
}

fn turn_bits(rotation: Rotation) -> u8 {
    match rotation {
        Rotation::R0 => 0,
        Rotation::R90 => MADCTL_MV | MADCTL_MY,
        Rotation::R180 => MADCTL_MX | MADCTL_MY,
        Rotation::R270 => MADCTL_MV | MADCTL_MX,
    }
}

fn span(start: i16, len: i16) -> [u8; 4] {
    let (start, end) = (start as u16, (start + len - 1) as u16);
    let (s, e) = (start.to_be_bytes(), end.to_be_bytes());
    [s[0], s[1], e[0], e[1]]
}

fn window(io: &IoGuard, area: Rect) -> Result<()> {
    io.tx_param(sys::LCD_CMD_CASET as i32, &span(area.x, area.w))?;
    io.tx_param(sys::LCD_CMD_RASET as i32, &span(area.y, area.h))
}

impl SpiLcd {
    pub(crate) fn new(config: Config) -> Result<Self> {
        let io = Io::new(&config.bus, &IoConfig {
            cs: config.cs,
            dc: config.dc,
            pclk_hz: config.pclk_hz,
            queue_depth: QUEUE_DEPTH,
            cmd_bits: 8,
            param_bits: 8,
        })
        .inspect_err(|err| error!(TAG, "panel io: {err}"))?;
        (config.init)(&io.lock()).inspect_err(|err| error!(TAG, "init: {err}"))?;
        let mut lcd = Self {
            io,
            size: config.size,
            format: config.pixel_format,
            madctl: config.madctl,
            rotation: Rotation::R0,
            backlight: config.backlight,
        };
        lcd.turn(config.rotation)?;
        Ok(lcd)
    }

    fn turn(&mut self, rotation: Rotation) -> Result<()> {
        let madctl = self.madctl ^ turn_bits(rotation);
        self.io
            .lock()
            .tx_param(sys::LCD_CMD_MADCTL as i32, &[madctl])
            .inspect_err(|err| error!(TAG, "rotation {rotation:?}: {err}"))?;
        self.rotation = rotation;
        Ok(())
    }
}

impl Driver for SpiLcd {
    fn size(&self) -> Size {
        if self.rotation.swaps_axes() { Size::new(self.size.height, self.size.width) } else { self.size }
    }

    fn pixel_format(&self) -> ImageFormat {
        self.format
    }

    fn rotation(&self) -> Rotation {
        self.rotation
    }

    unsafe fn write(&self, area: Rect, pixels: &[u8]) -> Result<Ticket> {
        let mut io = self.io.lock();
        window(&io, area)?;
        Ok(Ticket(unsafe { io.tx_color(sys::LCD_CMD_RAMWR as i32, pixels) }?))
    }

    fn wait(&self, ticket: Ticket) -> Result<()> {
        self.io.wait(ticket.0);
        Ok(())
    }

    fn draw(&self, area: Rect, image: &Image) -> Result<()> {
        let row = self.format.row_bytes(area.w as usize);
        let stride = image.stride_bytes();
        let rows = area.h as usize;
        if stride == row {
            let ticket = unsafe { self.write(area, &image.pixels()[..row * rows]) }?;
            return self.wait(ticket);
        }
        let mut io = self.io.lock();
        window(&io, area)?;
        let mut last = None;
        let mut result = Ok(());
        for j in 0..rows {
            let cmd = if j == 0 { sys::LCD_CMD_RAMWR } else { sys::LCD_CMD_RAMWRC };
            match unsafe { io.tx_color(cmd as i32, &image.pixels()[j * stride..][..row]) } {
                Ok(ticket) => last = Some(ticket),
                Err(err) => {
                    result = Err(err);
                    break;
                }
            }
        }
        drop(io);
        if let Some(ticket) = last {
            self.io.wait(ticket);
        }
        result
    }

    fn set_brightness(&self, level: u8) -> Result<()> {
        let backlight = self.backlight.as_ref().ok_or(Error::NOT_SUPPORTED)?;
        backlight(level.min(100))
    }

    fn reconfigure(&mut self, format: ImageFormat, fb_num: u8, rotation: Rotation) -> Result<()> {
        if format != self.format || fb_num != 0 {
            return Err(Error::NOT_SUPPORTED);
        }
        if rotation == self.rotation {
            return Ok(());
        }
        self.turn(rotation)
    }
}
