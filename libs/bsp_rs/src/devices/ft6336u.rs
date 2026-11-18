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

const TAG: &CStr = c"ft6336u";

pub(crate) const ADDR: u16 = 0x38;
pub(crate) const DEFAULT_HZ: u32 = 400_000;
const MAX_POINTS: usize = 2;

const REG_TD_STATUS: u8 = 0x02;
const REG_CHIP_ID: u8 = 0xA3;
const REG_G_MODE: u8 = 0xA4;
const G_MODE_TRIGGER: u8 = 0x01;
const POINT_SIZE: usize = 6;
const I2C_TIMEOUT: Duration = Duration::from_millis(50);

pub(crate) struct Ft6336u {
    dev: Device<'static>,
    points: [Point; MAX_POINTS],
}

impl Ft6336u {
    pub(crate) fn new(bus: &'static MasterBus, clock_hz: u32) -> Result<Self> {
        let ft = Self { dev: bus.add_device(ADDR, clock_hz)?, points: [Point::default(); MAX_POINTS] };
        let mut id = [0u8];
        ft.dev
            .transmit_receive(&[REG_CHIP_ID], &mut id, I2C_TIMEOUT)
            .inspect_err(|err| error!(TAG, "not responding on 0x{ADDR:02x}: {err}"))?;
        // The default polling mode holds INT low while touched, so an edge
        // driven host would see a single edge per touch.
        ft.dev.transmit(&[REG_G_MODE, G_MODE_TRIGGER], I2C_TIMEOUT)?;
        info!(TAG, "chip id 0x{:02x}", id[0]);
        Ok(ft)
    }
}

impl Driver for Ft6336u {
    fn poll(&mut self) -> Result<&[Point]> {
        let mut raw = [0u8; 1 + MAX_POINTS * POINT_SIZE];
        self.dev.transmit_receive(&[REG_TD_STATUS], &mut raw, I2C_TIMEOUT)?;
        let n = usize::from(raw[0] & 0x0F).min(MAX_POINTS);
        for (p, r) in self.points.iter_mut().zip(raw[1..].chunks_exact(POINT_SIZE)).take(n) {
            *p = Point {
                x: u16::from(r[0] & 0x0F) << 8 | u16::from(r[1]),
                y: u16::from(r[2] & 0x0F) << 8 | u16::from(r[3]),
                id: r[2] >> 4,
            };
        }
        Ok(&self.points[..n])
    }
}
