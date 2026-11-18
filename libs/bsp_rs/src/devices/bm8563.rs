/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;
use core::time::Duration;

use idf::i2c::{Device, MasterBus};
use idf::log::{error, warn};
use idf::{Error, Result};

use crate::rtc::{DateTime, Driver};

const TAG: &CStr = c"bm8563";

const REG_CTRL1: u8 = 0x00;
const REG_CTRL2: u8 = 0x01;
const REG_SECONDS: u8 = 0x02;
const REG_TIMER_CTRL: u8 = 0x0E;
const REG_TIMER_COUNT: u8 = 0x0F;

const SECONDS_VL: u8 = 0x80;
const CTRL2_TI_TP: u8 = 0x10;
const CTRL2_TF: u8 = 0x04;
const CTRL2_TIE: u8 = 0x01;
const TIMER_ENABLE: u8 = 0x80;
const TIMER_4096HZ: u8 = 0x00;
const TIMER_64HZ: u8 = 0x01;
const TIMER_1HZ: u8 = 0x02;
const TIMER_1_60HZ: u8 = 0x03;

const BASE_YEAR: u16 = 2000;
const MAX_YEAR: u16 = 2099;
const I2C_TIMEOUT: Duration = Duration::from_millis(50);

pub(crate) const ADDR: u16 = 0x51;
pub(crate) const DEFAULT_HZ: u32 = 400_000;

const fn bcd_to_dec(b: u8) -> u8 {
    (b >> 4) * 10 + (b & 0x0f)
}

const fn dec_to_bcd(d: u8) -> u8 {
    ((d / 10) << 4) | (d % 10)
}

// The finest source whose 8-bit count spans `ms`, rounded to the nearest tick.
fn pick_source(ms: u64) -> Option<(u8, u8)> {
    let (source, count) = match ms {
        0 => return None,
        ..=62 => (TIMER_4096HZ, (ms * 4096 + 500) / 1000),
        ..=3_984 => (TIMER_64HZ, (ms * 64 + 500) / 1000),
        ..=255_000 => (TIMER_1HZ, (ms + 500) / 1000),
        ..=15_300_000 => (TIMER_1_60HZ, (ms + 30_000) / 60_000),
        _ => return None,
    };
    Some((source, count.clamp(1, 255) as u8))
}

pub(crate) struct Bm8563 {
    device: Device<'static>,
}

impl Bm8563 {
    pub(crate) fn new(bus: &'static MasterBus, address: u16) -> Result<Self> {
        bus.probe(address, I2C_TIMEOUT).inspect_err(|err| error!(TAG, "no BM8563 at 0x{address:02x}: {err}"))?;
        let chip = Self { device: bus.add_device(address, DEFAULT_HZ)? };
        chip.init().inspect_err(|err| error!(TAG, "init: {err}"))?;
        Ok(chip)
    }

    fn read(&self, reg: u8, buf: &mut [u8]) -> Result<()> {
        self.device.transmit_receive(&[reg], buf, I2C_TIMEOUT)
    }

    fn read_reg(&self, reg: u8) -> Result<u8> {
        let mut value = [0u8];
        self.read(reg, &mut value)?;
        Ok(value[0])
    }

    fn write_reg(&self, reg: u8, value: u8) -> Result<()> {
        self.device.transmit(&[reg, value], I2C_TIMEOUT)
    }

    fn update_reg(&self, reg: u8, clear: u8, set: u8) -> Result<()> {
        let value = self.read_reg(reg)?;
        self.write_reg(reg, (value & !clear) | set)
    }

    // VL stays set so `is_valid` reports the loss until `set`.
    fn init(&self) -> Result<()> {
        if self.read_reg(REG_SECONDS)? & SECONDS_VL != 0 {
            warn!(TAG, "clock integrity lost, time invalid");
        }
        self.write_reg(REG_CTRL1, 0)
    }
}

impl Driver for Bm8563 {
    fn now(&mut self) -> Result<DateTime> {
        let mut b = [0u8; 7];
        self.read(REG_SECONDS, &mut b)?;
        Ok(DateTime {
            year: BASE_YEAR + u16::from(bcd_to_dec(b[6])),
            month: bcd_to_dec(b[5] & 0x1f),
            day: bcd_to_dec(b[3] & 0x3f),
            hour: bcd_to_dec(b[2] & 0x3f),
            minute: bcd_to_dec(b[1] & 0x7f),
            second: bcd_to_dec(b[0] & 0x7f),
        })
    }

    // Writing the seconds clears VL. The century bit stays 0, as boards read it
    // differently.
    fn set(&mut self, time: &DateTime) -> Result<()> {
        if !(BASE_YEAR..=MAX_YEAR).contains(&time.year) {
            return Err(Error::INVALID_ARG);
        }
        self.device.transmit(
            &[
                REG_SECONDS,
                dec_to_bcd(time.second),
                dec_to_bcd(time.minute),
                dec_to_bcd(time.hour),
                dec_to_bcd(time.day),
                time.weekday(),
                dec_to_bcd(time.month),
                dec_to_bcd((time.year - BASE_YEAR) as u8),
            ],
            I2C_TIMEOUT,
        )
    }

    fn is_valid(&mut self) -> Result<bool> {
        Ok(self.read_reg(REG_SECONDS)? & SECONDS_VL == 0)
    }

    // INT follows TF rather than pulsing, so it stays low until the flag is
    // cleared, long enough for whatever the board wires it to.
    fn start_timer(&mut self, period: Duration) -> Result<()> {
        let (source, count) = pick_source(period.as_millis() as u64).ok_or(Error::INVALID_ARG)?;
        self.write_reg(REG_TIMER_CTRL, 0)?;
        self.update_reg(REG_CTRL2, CTRL2_TF | CTRL2_TI_TP, CTRL2_TIE)?;
        self.write_reg(REG_TIMER_COUNT, count)?;
        self.write_reg(REG_TIMER_CTRL, TIMER_ENABLE | source)
    }

    fn stop_timer(&mut self) -> Result<()> {
        self.write_reg(REG_TIMER_CTRL, 0)?;
        self.update_reg(REG_CTRL2, CTRL2_TF | CTRL2_TIE, 0)
    }

    fn timer_expired(&mut self) -> Result<bool> {
        Ok(self.read_reg(REG_CTRL2)? & CTRL2_TF != 0)
    }

    fn clear_timer(&mut self) -> Result<()> {
        self.update_reg(REG_CTRL2, CTRL2_TF, 0)
    }
}
