/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! The LilyGo T5 4.7" S3: the same glass as the PaperS3, with CKV on a GPIO
//! and the power, STV, MODE, OE and LE behind a 74HCT4094 shift register.

use core::sync::atomic::{AtomicU8, Ordering};

use idf::time::delay_us;
use idf::{Result, gpio};

use crate::driver::epd_ll::Control;

pub(crate) const PINS_DATA: [i32; 8] = [8, 1, 2, 3, 4, 5, 6, 7];
pub(crate) const PIN_SPH: i32 = 40;
pub(crate) const PIN_CL: i32 = 41;
pub(crate) const PIN_DC_DUMMY: i32 = 45;

const PIN_SR_DATA: i32 = 13;
const PIN_SR_CLK: i32 = 12;
const PIN_SR_STR: i32 = 0;
const PIN_CKV: i32 = 38;

const LE: u8 = 1 << 0;
const POWER_DISABLE: u8 = 1 << 1;
const POS_ENABLE: u8 = 1 << 2;
const NEG_ENABLE: u8 = 1 << 3;
const STV: u8 = 1 << 4;
const POWER_ENABLE: u8 = 1 << 5;
const MODE: u8 = 1 << 6;
const OE: u8 = 1 << 7;

// The pins are outputs since `init`, so setting one cannot fail.
fn level(pin: i32, high: bool) {
    let _ = gpio::set_level(pin, high);
}

/// What the shift register holds.
pub(crate) struct ShiftControl(AtomicU8);

impl ShiftControl {
    pub(crate) fn new() -> Self {
        Self(AtomicU8::new(0))
    }

    /// QP7 first, then latches all eight together.
    fn push(&self, value: u8) {
        level(PIN_SR_STR, false);
        for bit in (0..8).rev() {
            level(PIN_SR_CLK, false);
            level(PIN_SR_DATA, value & 1 << bit != 0);
            level(PIN_SR_CLK, true);
        }
        level(PIN_SR_STR, true);
    }

    fn store(&self, value: u8) {
        self.0.store(value, Ordering::Relaxed);
        self.push(value);
    }

    fn set(&self, mask: u8, on: bool) {
        let old = if on { self.0.fetch_or(mask, Ordering::Relaxed) } else { self.0.fetch_and(!mask, Ordering::Relaxed) };
        self.push(if on { old | mask } else { old & !mask });
    }

    fn latch_row(&self) {
        self.set(LE, true);
        self.set(LE, false);
    }
}

fn ckv_pulse(high_us: u32, low_us: u32) {
    level(PIN_CKV, true);
    if high_us != 0 {
        delay_us(high_us);
    }
    level(PIN_CKV, false);
    if low_us != 0 {
        delay_us(low_us);
    }
}

impl Control for ShiftControl {
    fn init(&self) -> Result<()> {
        let pins = [PIN_SR_DATA, PIN_SR_CLK, PIN_SR_STR, PIN_CKV];
        gpio::configure(&pins, gpio::Mode::Output)?;
        for pin in pins {
            level(pin, false);
        }
        self.store(0);
        Ok(())
    }

    fn power_on(&self) {
        self.store(POWER_ENABLE);
        delay_us(100);
        self.set(NEG_ENABLE, true);
        delay_us(500);
        self.set(POS_ENABLE, true);
        delay_us(100);
        self.set(STV, true);
    }

    fn power_off(&self) {
        self.set(POS_ENABLE, false);
        delay_us(10);
        self.set(NEG_ENABLE, false);
        delay_us(100);
        self.set(POWER_DISABLE, true);
        self.store(0);
    }

    fn frame_begin(&self) {
        self.set(MODE, true);
        ckv_pulse(1, 1);
        self.set(STV, false);
        delay_us(1);
        // STV has to rise while CKV is still high, or the gate driver misses
        // the start pulse.
        level(PIN_CKV, true);
        self.set(STV, true);
        delay_us(10);
        level(PIN_CKV, false);
        delay_us(10);
        ckv_pulse(10, 0);
        self.set(OE, true);
        ckv_pulse(1, 1);
    }

    // Each line latches the one sent before it, so the last is latched here.
    fn frame_end(&self) {
        self.latch_row();
        ckv_pulse(1, 1);
        self.set(OE, false);
        self.set(MODE, false);
        ckv_pulse(1, 1);
        ckv_pulse(1, 1);
    }

    fn line_begin(&self) {
        self.latch_row();
        level(PIN_CKV, true);
    }

    fn line_latch(&self) {
        level(PIN_CKV, false);
    }
}
