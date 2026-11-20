/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;
use core::time::Duration;

use idf::i2c::{Device, MasterBus};
use idf::log::error;
use idf::time::Instant;
use idf::{Error, Result, thread};

const TAG: &CStr = c"m5ioe1";

pub(crate) const DEFAULT_HZ: u32 = 100_000;

const TIMEOUT: Duration = Duration::from_millis(50);
const BOOT_TIMEOUT: Duration = Duration::from_millis(500);
const BOOT_POLL: Duration = Duration::from_millis(10);

const REG_REV: u8 = 0x02;
const REG_MODE: u8 = 0x03;
const REG_OUTPUT: u8 = 0x05;
const REG_INPUT: u8 = 0x07;
const REG_DRIVE: u8 = 0x13;
const REG_I2C_CFG: u8 = 0x23;

const PINS: u8 = 14;

/// Pins are numbered 1..=14, as the chip's IO1..IO14.
pub(crate) struct M5Ioe1<'bus> {
    device: Device<'bus>,
    mode: u16,
    output: u16,
    drive: u16,
}

fn mask(pin: u8) -> Result<u16> {
    if !(1..=PINS).contains(&pin) {
        return Err(Error::INVALID_ARG);
    }
    Ok(1 << (pin - 1))
}

impl<'bus> M5Ioe1<'bus> {
    /// Waits for the chip to boot, and keeps it from sleeping between
    /// transfers. Pins keep what they were set to before.
    pub(crate) fn new(bus: &'bus MasterBus, address: u16) -> Result<Self> {
        let device = bus.add_device(address, DEFAULT_HZ)?;
        let start = Instant::now();
        let mut rev = [0u8];
        while let Err(err) = device.transmit_receive(&[REG_REV], &mut rev, TIMEOUT) {
            if start.elapsed() >= BOOT_TIMEOUT {
                error!(TAG, "not responding on 0x{address:02x}: {err}");
                return Err(err);
            }
            thread::sleep(BOOT_POLL);
        }
        device.transmit(&[REG_I2C_CFG, 0x00], TIMEOUT)?;
        let mut ioe = Self { device, mode: 0, output: 0, drive: 0 };
        ioe.mode = ioe.read_pair(REG_MODE)?;
        ioe.output = ioe.read_pair(REG_OUTPUT)?;
        ioe.drive = ioe.read_pair(REG_DRIVE)?;
        Ok(ioe)
    }

    /// A push-pull output, at `level` from the moment it drives.
    pub(crate) fn set_output_mode(&mut self, pin: u8, level: bool) -> Result<()> {
        let mask = mask(pin)?;
        self.update(REG_OUTPUT, |s| &mut s.output, mask, level)?;
        self.update(REG_DRIVE, |s| &mut s.drive, mask, false)?;
        self.update(REG_MODE, |s| &mut s.mode, mask, true)
    }

    pub(crate) fn set_output(&mut self, pin: u8, level: bool) -> Result<()> {
        let mask = mask(pin)?;
        if self.mode & mask == 0 {
            error!(TAG, "IO{pin} is not an output");
            return Err(Error::INVALID_STATE);
        }
        self.update(REG_OUTPUT, |s| &mut s.output, mask, level)
    }

    /// The level on `pin`, whether it drives or not.
    pub(crate) fn level(&self, pin: u8) -> Result<bool> {
        let mask = mask(pin)?;
        Ok(self.read_pair(REG_INPUT)? & mask != 0)
    }

    fn update(&mut self, reg: u8, field: fn(&mut Self) -> &mut u16, mask: u16, set: bool) -> Result<()> {
        let old = *field(self);
        let new = if set { old | mask } else { old & !mask };
        if new != old {
            let [lo, hi] = new.to_le_bytes();
            let (reg, value) = if mask & 0xFF != 0 { (reg, lo) } else { (reg + 1, hi) };
            self.device.transmit(&[reg, value], TIMEOUT)?;
            *field(self) = new;
        }
        Ok(())
    }

    fn read_pair(&self, reg: u8) -> Result<u16> {
        let mut value = [0u8; 2];
        self.device.transmit_receive(&[reg], &mut value, TIMEOUT)?;
        Ok(u16::from_le_bytes(value))
    }
}
