/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::time::Duration;

use idf::i2c::{Device, MasterBus};
use idf::{Result, gpio};

use crate::touch::{Driver, Point};

pub(crate) const ADDR: u16 = 0x55;
pub(crate) const DEFAULT_HZ: u32 = 100_000;
const MAX_POINTS: usize = 10;

const REG_MAX_TOUCHES: u16 = 0x0009;
const REG_ADV_INFO: u16 = 0x0010;
const REG_REPORT_COORD: u16 = 0x0014;
const ADV_INFO_WITH_COORD: u8 = 0x08;
const POINT_SIZE: usize = 7;
const I2C_TIMEOUT: Duration = Duration::from_millis(50);

pub(crate) struct Config {
    pub clock_hz: u32,
    pub int_pin: Option<i32>,
}

pub(crate) struct St712xTouch {
    dev: Device<'static>,
    points: [Point; MAX_POINTS],
    count: usize,
}

impl St712xTouch {
    /// The board resets the chip beforehand.
    pub(crate) fn new(bus: &'static MasterBus, config: &Config) -> Result<Self> {
        if let Some(pin) = config.int_pin {
            gpio::set_direction(pin, gpio::Mode::Input)?;
            gpio::set_pull(pin, gpio::Pull::Up)?;
        }
        Ok(Self { dev: bus.add_device(ADDR, config.clock_hz)?, points: [Point::default(); MAX_POINTS], count: 0 })
    }

    fn read_reg(&self, reg: u16, buf: &mut [u8]) -> Result<()> {
        self.dev.transmit_receive(&reg.to_be_bytes(), buf, I2C_TIMEOUT)
    }
}

impl Driver for St712xTouch {
    fn poll(&mut self) -> Result<&[Point]> {
        let mut adv = [0u8];
        self.read_reg(REG_ADV_INFO, &mut adv)?;
        if adv[0] & ADV_INFO_WITH_COORD != 0 {
            let mut max = [0u8];
            self.read_reg(REG_MAX_TOUCHES, &mut max)?;
            let reports = usize::from(max[0]).min(MAX_POINTS);
            let mut raw = [0u8; MAX_POINTS * POINT_SIZE];
            if reports > 0 {
                self.read_reg(REG_REPORT_COORD, &mut raw[..reports * POINT_SIZE])?;
            }
            let valid = raw.chunks_exact(POINT_SIZE).take(reports).filter(|r| r[0] & 0x80 != 0);
            self.count = 0;
            for r in valid {
                // Reports carry no track id; the index among valid reports stands in.
                self.points[self.count] = Point {
                    x: u16::from(r[0] & 0x3F) << 8 | u16::from(r[1]),
                    y: u16::from(r[2]) << 8 | u16::from(r[3]),
                    id: self.count as u8,
                };
                self.count += 1;
            }
        }
        Ok(&self.points[..self.count])
    }
}
