/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;
use core::time::Duration;

use idf::i2c::{Device, MasterBus};
use idf::log::{error, info};
use idf::{Error, Result, gpio};

use crate::touch::{Driver, Point};

const TAG: &CStr = c"gt911";

pub(crate) const ADDR_PRIMARY: u16 = 0x14;
pub(crate) const ADDR_SECONDARY: u16 = 0x5D;
pub(crate) const DEFAULT_HZ: u32 = 400_000;
const MAX_POINTS: usize = 5;

const REG_PRODUCT_ID: u16 = 0x8140;
const REG_STATUS: u16 = 0x814E;
const REG_TOUCH_DATA: u16 = 0x814F;
const STATUS_READY: u8 = 0x80;
const STATUS_COUNT_MASK: u8 = 0x0F;
const POINT_SIZE: usize = 8;
const I2C_TIMEOUT: Duration = Duration::from_millis(50);

pub(crate) struct Config {
    /// Tried first; the other address is probed next.
    pub address: u16,
    pub clock_hz: u32,
    pub int_pin: Option<i32>,
}

pub(crate) struct Gt911 {
    dev: Device<'static>,
    points: [Point; MAX_POINTS],
    count: usize,
}

impl Gt911 {
    /// The board resets the chip (latching its address from INT) beforehand.
    pub(crate) fn new(bus: &'static MasterBus, config: &Config) -> Result<Self> {
        if let Some(pin) = config.int_pin {
            gpio::set_direction(pin, gpio::Mode::Input)?;
            gpio::set_pull(pin, gpio::Pull::Up)?;
        }
        let other = if config.address == ADDR_PRIMARY { ADDR_SECONDARY } else { ADDR_PRIMARY };
        let address = [config.address, other]
            .into_iter()
            .find(|&a| bus.probe(a, I2C_TIMEOUT).is_ok())
            .ok_or(Error::NOT_FOUND)
            .inspect_err(|_| error!(TAG, "no GT911 on bus"))?;
        let gt911 = Self { dev: bus.add_device(address, config.clock_hz)?, points: [Point::default(); MAX_POINTS], count: 0 };
        gt911.log_info(address);
        Ok(gt911)
    }

    fn read_reg(&self, reg: u16, buf: &mut [u8]) -> Result<()> {
        self.dev.transmit_receive(&reg.to_be_bytes(), buf, I2C_TIMEOUT)
    }

    fn write_byte(&self, reg: u16, value: u8) -> Result<()> {
        let [hi, lo] = reg.to_be_bytes();
        self.dev.transmit(&[hi, lo, value], I2C_TIMEOUT)
    }

    fn log_info(&self, address: u16) {
        let mut b = [0u8; 11];
        if self.read_reg(REG_PRODUCT_ID, &mut b).is_err() {
            return;
        }
        let id = core::str::from_utf8(&b[..4]).unwrap_or("?").trim_end_matches('\0');
        let fw = u16::from_le_bytes([b[4], b[5]]);
        let (xr, yr) = (u16::from_le_bytes([b[6], b[7]]), u16::from_le_bytes([b[8], b[9]]));
        info!(TAG, "id={id} fw=0x{fw:04x} res={xr}x{yr} vendor=0x{:02x} @0x{address:02x}", b[10]);
    }
}

impl Driver for Gt911 {
    fn poll(&mut self) -> Result<&[Point]> {
        let mut status = [0u8];
        self.read_reg(REG_STATUS, &mut status)?;
        if status[0] & STATUS_READY != 0 {
            let n = usize::from(status[0] & STATUS_COUNT_MASK).min(MAX_POINTS);
            let mut raw = [0u8; MAX_POINTS * POINT_SIZE];
            let read = if n > 0 { self.read_reg(REG_TOUCH_DATA, &mut raw[..n * POINT_SIZE]) } else { Ok(()) };
            // Cleared even after a failed read so a glitch does not leave the
            // latch set and the stale frame reported forever.
            self.write_byte(REG_STATUS, 0)?;
            read?;
            for (p, r) in self.points.iter_mut().zip(raw.chunks_exact(POINT_SIZE)).take(n) {
                *p = Point {
                    x: u16::from_le_bytes([r[1], r[2]]),
                    y: u16::from_le_bytes([r[3], r[4]]),
                    id: r[0],
                };
            }
            self.count = n;
        }
        Ok(&self.points[..self.count])
    }
}
