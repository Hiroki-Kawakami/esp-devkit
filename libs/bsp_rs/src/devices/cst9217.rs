/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;
use core::time::Duration;

use idf::i2c::{Device, MasterBus};
use idf::log::{error, info};
use idf::{Error, Result, thread};

use crate::touch::{Driver, Point};

const TAG: &CStr = c"cst9217";

pub(crate) const ADDR: u16 = 0x5A;
pub(crate) const DEFAULT_HZ: u32 = 400_000;
const MAX_POINTS: usize = 2;

const CMD_DEBUG: u16 = 0xD101;
const CMD_NORMAL: u16 = 0xD109;
const REG_CHECK: u16 = 0xD1FC;
const REG_CHIP_TYPE: u16 = 0xD204;
const REG_REPORT: u16 = 0xD000;
const CHECK_MARK: u8 = 0xCA;
const CHIP_TYPE: u16 = 0x9217;
const FRAME_MARK: u8 = 0xAB;
const EVENT_PRESSED: u8 = 0x06;
const REPORT_SIZE: usize = MAX_POINTS * 5 + 5;
const DEBUG_SETTLE: Duration = Duration::from_millis(10);
const NORMAL_SETTLE: Duration = Duration::from_millis(5);
const I2C_TIMEOUT: Duration = Duration::from_millis(50);

pub(crate) struct Cst9217 {
    dev: Device<'static>,
    points: [Point; MAX_POINTS],
    count: usize,
}

impl Cst9217 {
    /// The chip answers up to 50 ms after its reset ends.
    pub(crate) fn new(bus: &'static MasterBus, clock_hz: u32) -> Result<Self> {
        let cst = Self { dev: bus.add_device(ADDR, clock_hz)?, points: [Point::default(); MAX_POINTS], count: 0 };
        cst.identify().inspect_err(|err| error!(TAG, "not responding on 0x{ADDR:02x}: {err}"))?;
        Ok(cst)
    }

    // The identification registers read back only in debug mode.
    fn identify(&self) -> Result<()> {
        self.command(CMD_DEBUG)?;
        thread::sleep(DEBUG_SETTLE);
        let mut b = [0u8; 4];
        self.read(REG_CHECK, &mut b)?;
        if b[2] != CHECK_MARK || b[3] != CHECK_MARK {
            return Err(Error::INVALID_RESPONSE);
        }
        self.read(REG_CHIP_TYPE, &mut b)?;
        let chip = u16::from_le_bytes([b[2], b[3]]);
        if chip != CHIP_TYPE {
            error!(TAG, "chip type 0x{chip:04x}");
            return Err(Error::NOT_FOUND);
        }
        self.command(CMD_NORMAL)?;
        thread::sleep(NORMAL_SETTLE);
        info!(TAG, "chip type 0x{chip:04x}");
        Ok(())
    }

    fn command(&self, cmd: u16) -> Result<()> {
        self.dev.transmit(&cmd.to_be_bytes(), I2C_TIMEOUT)
    }

    fn read(&self, reg: u16, buf: &mut [u8]) -> Result<()> {
        self.dev.transmit_receive(&reg.to_be_bytes(), buf, I2C_TIMEOUT)
    }
}

impl Driver for Cst9217 {
    fn poll(&mut self) -> Result<&[Point]> {
        let mut raw = [0u8; REPORT_SIZE];
        let read = self.read(REG_REPORT, &mut raw);
        // The mark written over the first byte tells the next poll whether the
        // chip has reported since.
        let [hi, lo] = REG_REPORT.to_be_bytes();
        self.dev.transmit(&[hi, lo, FRAME_MARK], I2C_TIMEOUT)?;
        read?;
        if raw[0] == FRAME_MARK {
            return Ok(&self.points[..self.count]);
        }
        if raw[6] != FRAME_MARK {
            return Err(Error::INVALID_RESPONSE);
        }
        let slots = usize::from(raw[5] & 0x7F).min(MAX_POINTS);
        let mut n = 0;
        for i in 0..slots {
            let r = &raw[i * 5 + if i > 0 { 2 } else { 0 }..][..4];
            let id = r[0] >> 4;
            if r[0] & 0x0F != EVENT_PRESSED || self.points[..n].iter().any(|p| p.id == id) {
                continue;
            }
            self.points[n] = Point {
                x: u16::from(r[1]) << 4 | u16::from(r[3] >> 4),
                y: u16::from(r[2]) << 4 | u16::from(r[3] & 0x0F),
                id,
            };
            n += 1;
        }
        self.count = n;
        Ok(&self.points[..n])
    }
}
