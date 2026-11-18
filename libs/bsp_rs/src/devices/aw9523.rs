/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;
use core::time::Duration;

use idf::i2c::{Device, MasterBus};
use idf::log::error;
use idf::{Error, Result};

const TAG: &CStr = c"aw9523";

pub(crate) const DEFAULT_HZ: u32 = 400_000;

const TIMEOUT: Duration = Duration::from_millis(100);

const REG_OUTPUT0: u8 = 0x02;
const REG_CONFIG0: u8 = 0x04;
const REG_INT0: u8 = 0x06;
const REG_ID: u8 = 0x10;
const REG_CTL: u8 = 0x11;
const REG_LEDMODE0: u8 = 0x12;
const REG_SOFT_RST: u8 = 0x7F;

const CTL_P0_PUSH_PULL: u8 = 0x10;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(crate) enum PinMode {
    #[default]
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

/// Pins 0..=7 are port 0, 8..=15 port 1.
pub(crate) struct Aw9523<'bus> {
    device: Device<'bus>,
    direction: u16,
    output: u16,
}

impl<'bus> Aw9523<'bus> {
    /// Every pin is a GPIO with its interrupt masked, port 0 push-pull.
    pub(crate) fn new(bus: &'bus MasterBus, address: u16, config: &[PinConfig; 16]) -> Result<Self> {
        let device = bus.add_device(address, DEFAULT_HZ)?;
        let (mut direction, mut output) = (0u16, 0u16);
        for (i, pin) in config.iter().enumerate() {
            if pin.mode == PinMode::Output {
                direction |= 1 << i;
                if pin.initial {
                    output |= 1 << i;
                }
            }
        }
        let aw = Self { device, direction, output };
        aw.write_reg(REG_SOFT_RST, 0x00)?;
        aw.read_reg(REG_ID).inspect_err(|err| error!(TAG, "not responding on 0x{address:02x}: {err}"))?;
        let [out0, out1] = output.to_le_bytes();
        let [in0, in1] = (!direction).to_le_bytes();
        // Levels before directions, so no output glitches as it turns on.
        for (reg, value) in [
            (REG_LEDMODE0, 0xFF),
            (REG_LEDMODE0 + 1, 0xFF),
            (REG_CTL, CTL_P0_PUSH_PULL),
            (REG_OUTPUT0, out0),
            (REG_OUTPUT0 + 1, out1),
            (REG_CONFIG0, in0),
            (REG_CONFIG0 + 1, in1),
            (REG_INT0, 0xFF),
            (REG_INT0 + 1, 0xFF),
        ] {
            aw.write_reg(reg, value)?;
        }
        Ok(aw)
    }

    pub(crate) fn set_output(&mut self, pin: u8, value: bool) -> Result<()> {
        if pin >= 16 {
            return Err(Error::INVALID_ARG);
        }
        let mask = 1u16 << pin;
        if self.direction & mask == 0 {
            error!(TAG, "pin {pin} is not configured as output");
            return Err(Error::INVALID_STATE);
        }
        if (self.output & mask != 0) == value {
            return Ok(());
        }
        let output = if value { self.output | mask } else { self.output & !mask };
        let port = pin / 8;
        self.write_reg(REG_OUTPUT0 + port, output.to_le_bytes()[usize::from(port)])?;
        self.output = output;
        Ok(())
    }

    fn write_reg(&self, reg: u8, value: u8) -> Result<()> {
        self.device.transmit(&[reg, value], TIMEOUT)
    }

    fn read_reg(&self, reg: u8) -> Result<u8> {
        let mut value = [0u8];
        self.device.transmit_receive(&[reg], &mut value, TIMEOUT)?;
        Ok(value[0])
    }
}
