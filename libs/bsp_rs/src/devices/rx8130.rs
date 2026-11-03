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

const TAG: &CStr = c"rx8130";

const REG_SECONDS: u8 = 0x10;
const REG_TIMER_COUNT0: u8 = 0x1A;
const REG_EXTENSION: u8 = 0x1C;
const REG_FLAG: u8 = 0x1D;
const REG_CTRL0: u8 = 0x1E;
const REG_CTRL1: u8 = 0x1F;
const REG_DIGITAL_OFFSET: u8 = 0x30;

const EXT_TE: u8 = 0x10;
const EXT_TSEL_MASK: u8 = 0x07;
const TSEL_64HZ: u8 = 0x01;
const TSEL_1HZ: u8 = 0x02;
const TSEL_1_60HZ: u8 = 0x03;
const TSEL_1_3600HZ: u8 = 0x04;

// Flags are write-0-to-clear and writing 1 is ignored, so one write clears a
// flag without racing a newly raised one.
const FLAG_TF: u8 = 0x10;
const FLAG_VLF: u8 = 0x02;
const FLAG_WRITE_KEEP: u8 = 0xBF;

const CTRL0_TEST: u8 = 0x80;
const CTRL0_STOP: u8 = 0x40;
const CTRL0_TIE: u8 = 0x10;

const CTRL1_CHGEN: u8 = 0x20;
const CTRL1_INIEN: u8 = 0x10;

const BASE_YEAR: u16 = 2000;
const MAX_YEAR: u16 = 2099;
const I2C_TIMEOUT: Duration = Duration::from_millis(50);

pub(crate) const ADDR: u16 = 0x32;
pub(crate) const DEFAULT_HZ: u32 = 400_000;

pub(crate) struct Config {
    /// Charges the backup cell from VDD; only for a rechargeable one.
    pub charge_backup: bool,
    /// A shorter timer period is `NOT_SUPPORTED`.
    pub timer_min: Duration,
}

const fn bcd_to_dec(b: u8) -> u8 {
    (b >> 4) * 10 + (b & 0x0f)
}

const fn dec_to_bcd(d: u8) -> u8 {
    ((d / 10) << 4) | (d % 10)
}

// The finest source whose 16-bit count spans `ms`, rounded to the nearest tick.
// 4096 Hz is never used: its interrupt pulse is 122 us against 7.57 ms for the
// others, too short to latch a board power rail.
fn pick_source(ms: u64) -> Option<(u8, u16)> {
    let (tsel, tick_ms) = match ms {
        0 => return None,
        ..=1_023_984 => (TSEL_64HZ, 0),
        ..=65_535_000 => (TSEL_1HZ, 1000),
        ..=3_932_100_000 => (TSEL_1_60HZ, 60_000),
        ..=235_926_000_000 => (TSEL_1_3600HZ, 3_600_000),
        _ => return None,
    };
    let count = if tick_ms == 0 { (ms * 64 + 500) / 1000 } else { (ms + tick_ms / 2) / tick_ms };
    Some((tsel, count.clamp(1, 65_535) as u16))
}

pub(crate) struct Rx8130 {
    device: Device<'static>,
    timer_min: Duration,
}

impl Rx8130 {
    pub(crate) fn new(bus: &'static MasterBus, address: u16, config: &Config) -> Result<Self> {
        bus.probe(address, I2C_TIMEOUT).inspect_err(|err| error!(TAG, "no RX8130 at 0x{address:02x}: {err}"))?;
        let chip = Self { device: bus.add_device(address, DEFAULT_HZ)?, timer_min: config.timer_min };
        chip.init(config.charge_backup).inspect_err(|err| error!(TAG, "init: {err}"))?;
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

    fn clear_flag(&self, flag: u8) -> Result<()> {
        self.write_reg(REG_FLAG, FLAG_WRITE_KEEP & !flag)
    }

    // VLF stays set so `is_valid` reports the loss until `set`.
    fn init(&self, charge_backup: bool) -> Result<()> {
        if self.read_reg(REG_FLAG)? & FLAG_VLF != 0 {
            warn!(TAG, "oscillation stop detected, time invalid");
            self.write_reg(REG_DIGITAL_OFFSET, 0)?;
            self.update_reg(REG_EXTENSION, EXT_TE, 0)?;
            self.write_reg(REG_CTRL0, 0)?;
        }
        self.update_reg(REG_CTRL0, CTRL0_TEST | CTRL0_STOP, 0)?;
        let charge = if charge_backup { CTRL1_CHGEN } else { 0 };
        self.update_reg(REG_CTRL1, CTRL1_CHGEN | CTRL1_INIEN, CTRL1_INIEN | charge)
    }
}

impl Driver for Rx8130 {
    fn now(&mut self) -> Result<DateTime> {
        let mut b = [0u8; 7];
        self.read(REG_SECONDS, &mut b)?;
        Ok(DateTime {
            year: BASE_YEAR + u16::from(bcd_to_dec(b[6])),
            month: bcd_to_dec(b[5] & 0x1f),
            day: bcd_to_dec(b[4] & 0x3f),
            hour: bcd_to_dec(b[2] & 0x3f),
            minute: bcd_to_dec(b[1] & 0x7f),
            second: bcd_to_dec(b[0] & 0x7f),
        })
    }

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
                1 << time.weekday(),
                dec_to_bcd(time.day),
                dec_to_bcd(time.month),
                dec_to_bcd((time.year - BASE_YEAR) as u8),
            ],
            I2C_TIMEOUT,
        )?;
        self.clear_flag(FLAG_VLF)
    }

    fn is_valid(&mut self) -> Result<bool> {
        Ok(self.read_reg(REG_FLAG)? & FLAG_VLF == 0)
    }

    fn start_timer(&mut self, period: Duration) -> Result<()> {
        if period < self.timer_min {
            return Err(Error::NOT_SUPPORTED);
        }
        let (tsel, count) = pick_source(period.as_millis() as u64).ok_or(Error::INVALID_ARG)?;
        let ext = (self.read_reg(REG_EXTENSION)? & !(EXT_TE | EXT_TSEL_MASK)) | tsel;
        self.write_reg(REG_EXTENSION, ext)?;
        self.clear_flag(FLAG_TF)?;
        self.update_reg(REG_CTRL0, CTRL0_TEST, CTRL0_TIE)?;
        let [lo, hi] = count.to_le_bytes();
        self.device.transmit(&[REG_TIMER_COUNT0, lo, hi], I2C_TIMEOUT)?;
        self.write_reg(REG_EXTENSION, ext | EXT_TE)
    }

    fn stop_timer(&mut self) -> Result<()> {
        self.update_reg(REG_EXTENSION, EXT_TE, 0)?;
        self.update_reg(REG_CTRL0, CTRL0_TEST | CTRL0_TIE, 0)?;
        self.clear_flag(FLAG_TF)
    }

    fn timer_expired(&mut self) -> Result<bool> {
        Ok(self.read_reg(REG_FLAG)? & FLAG_TF != 0)
    }

    fn clear_timer(&mut self) -> Result<()> {
        self.clear_flag(FLAG_TF)
    }
}

