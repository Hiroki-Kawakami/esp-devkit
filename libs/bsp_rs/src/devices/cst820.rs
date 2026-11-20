/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;
use core::time::Duration;

use idf::i2c::{Device, MasterBus};
use idf::log::{error, info};
use idf::Result;

use crate::touch::{Driver, Point};

const TAG: &CStr = c"cst820";

pub(crate) const ADDR: u16 = 0x15;
pub(crate) const DEFAULT_HZ: u32 = 400_000;

const REG_FINGERS: u8 = 0x02;
const REG_CHIP_ID: u8 = 0xA7;
const REG_PULSE_WIDTH: u8 = 0xED;
const REG_IRQ_CTL: u8 = 0xFA;
const IRQ_ON_CHANGE: u8 = 0x20;
// In 0.1 ms.
const PULSE_WIDTH: u8 = 20;
const EVENT_UP: u8 = 1;
const I2C_TIMEOUT: Duration = Duration::from_millis(50);

pub(crate) struct Cst820 {
    dev: Device<'static>,
    point: [Point; 1],
}

impl Cst820 {
    /// The chip answers only for a while after a reset or a touch, so this
    /// follows a reset.
    pub(crate) fn new(bus: &'static MasterBus, clock_hz: u32) -> Result<Self> {
        let cst = Self { dev: bus.add_device(ADDR, clock_hz)?, point: [Point::default()] };
        let mut id = [0u8; 3];
        cst.dev
            .transmit_receive(&[REG_CHIP_ID], &mut id, I2C_TIMEOUT)
            .inspect_err(|err| error!(TAG, "not responding on 0x{ADDR:02x}: {err}"))?;
        // Pulsing INT all through a touch, its other mode, degrades the
        // coordinates while the line is low.
        cst.dev.transmit(&[REG_IRQ_CTL, IRQ_ON_CHANGE], I2C_TIMEOUT)?;
        cst.dev.transmit(&[REG_PULSE_WIDTH, PULSE_WIDTH], I2C_TIMEOUT)?;
        info!(TAG, "chip id 0x{:02x} project 0x{:02x} fw 0x{:02x}", id[0], id[1], id[2]);
        Ok(cst)
    }
}

impl Driver for Cst820 {
    fn poll(&mut self) -> Result<&[Point]> {
        let mut raw = [0u8; 5];
        self.dev.transmit_receive(&[REG_FINGERS], &mut raw, I2C_TIMEOUT)?;
        if raw[0] & 0x0F == 0 || raw[1] >> 6 == EVENT_UP {
            return Ok(&[]);
        }
        self.point[0] = Point {
            x: u16::from(raw[1] & 0x0F) << 8 | u16::from(raw[2]),
            y: u16::from(raw[3] & 0x0F) << 8 | u16::from(raw[4]),
            id: 0,
        };
        Ok(&self.point)
    }
}
