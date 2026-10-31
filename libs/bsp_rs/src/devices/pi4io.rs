/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;

use idf::i2c::{self, Device, MasterBus};
use idf::log::error;
use idf::{Error, Result};

const TAG: &CStr = c"pi4io";

const REG_CHIP_RESET: u8 = 0x01;
const REG_IO_DIR: u8 = 0x03;
const REG_OUT_SET: u8 = 0x05;
const REG_OUT_H_IM: u8 = 0x07;
const REG_IN_DEF_STA: u8 = 0x09;
const REG_PULL_EN: u8 = 0x0B;
const REG_IN_STA: u8 = 0x0F;
const REG_INT_MASK: u8 = 0x11;

const SCL_HZ: u32 = 400_000;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(crate) enum PinMode {
    #[default]
    None,
    Input,
    Output,
}

#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct PinConfig {
    pub mode: PinMode,
    pub initial: bool,
}

impl PinConfig {
    pub(crate) const fn output(initial: bool) -> Self {
        Self { mode: PinMode::Output, initial }
    }

    pub(crate) const fn input() -> Self {
        Self { mode: PinMode::Input, initial: false }
    }
}

pub(crate) struct Pi4io<'bus> {
    device: Device<'bus>,
    direction: u8,
    output: u8,
}

impl<'bus> Pi4io<'bus> {
    pub(crate) fn new(bus: &'bus MasterBus, address: u16, config: &[PinConfig; 8]) -> Result<Self> {
        let device = bus.add_device(address, SCL_HZ)?;

        let (mut io_dir, mut in_def_sta, mut out_set) = (0u8, 0u8, 0u8);
        for (i, pin) in config.iter().enumerate() {
            let mask = 1 << i;
            match pin.mode {
                PinMode::Output => {
                    io_dir |= mask;
                    if pin.initial {
                        out_set |= mask;
                    }
                }
                PinMode::Input => {
                    if pin.initial {
                        in_def_sta |= mask;
                    }
                }
                PinMode::None => {}
            }
        }

        let pi4io = Self { device, direction: io_dir, output: out_set };
        pi4io.write_reg(REG_CHIP_RESET, 0xFF)?;
        pi4io.read_reg(REG_CHIP_RESET)?;
        if io_dir != 0x00 {
            pi4io.write_reg(REG_IO_DIR, io_dir)?;
        }
        pi4io.write_reg(REG_OUT_H_IM, 0x00)?;
        pi4io.write_reg(REG_PULL_EN, 0x00)?;
        if in_def_sta != 0x00 {
            pi4io.write_reg(REG_IN_DEF_STA, in_def_sta)?;
        }
        pi4io.write_reg(REG_INT_MASK, 0xFF)?;
        if out_set != 0x00 {
            pi4io.write_reg(REG_OUT_SET, out_set)?;
        }
        Ok(pi4io)
    }

    pub(crate) fn set_output(&mut self, pin: u8, value: bool) -> Result<()> {
        if pin >= 8 {
            return Err(Error::INVALID_ARG);
        }
        let mask = 1 << pin;
        if self.direction & mask == 0 {
            error!(TAG, "pin {pin} is not configured as output");
            return Err(Error::INVALID_STATE);
        }
        if (self.output & mask != 0) == value {
            return Ok(());
        }
        let output = if value { self.output | mask } else { self.output & !mask };
        self.write_reg(REG_OUT_SET, output)?;
        self.output = output;
        Ok(())
    }

    /// An output pin reads back its last written level.
    pub(crate) fn get_input(&self, pin: u8) -> Result<bool> {
        if pin >= 8 {
            return Err(Error::INVALID_ARG);
        }
        let mask = 1 << pin;
        if self.direction & mask != 0 {
            return Ok(self.output & mask != 0);
        }
        Ok(self.read_reg(REG_IN_STA)? & mask != 0)
    }

    fn write_reg(&self, reg: u8, value: u8) -> Result<()> {
        self.device.transmit(&[reg, value], i2c::FOREVER)
    }

    fn read_reg(&self, reg: u8) -> Result<u8> {
        let mut value = [0u8];
        self.device.transmit_receive(&[reg], &mut value, i2c::FOREVER)?;
        Ok(value[0])
    }
}
