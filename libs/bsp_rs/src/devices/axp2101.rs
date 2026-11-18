/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;
use core::time::Duration;

use idf::i2c::{Device, MasterBus};
use idf::log::error;
use idf::Result;

const TAG: &CStr = c"axp2101";

pub(crate) const ADDR: u16 = 0x34;
pub(crate) const DEFAULT_HZ: u32 = 400_000;

const TIMEOUT: Duration = Duration::from_millis(100);

const REG_STATUS1: u8 = 0x00;
const REG_LDO_EN0: u8 = 0x90;
const REG_LDO_VOLT0: u8 = 0x92;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Ldo {
    Aldo1,
    Aldo2,
    Aldo3,
    Aldo4,
    Bldo1,
    Bldo2,
    Dldo1,
}

impl Ldo {
    const fn index(self) -> u8 {
        match self {
            Self::Aldo1 => 0,
            Self::Aldo2 => 1,
            Self::Aldo3 => 2,
            Self::Aldo4 => 3,
            Self::Bldo1 => 4,
            Self::Bldo2 => 5,
            Self::Dldo1 => 7,
        }
    }
}

pub(crate) struct Axp2101<'bus> {
    device: Device<'bus>,
}

impl<'bus> Axp2101<'bus> {
    pub(crate) fn new(bus: &'bus MasterBus, address: u16, clock_hz: u32) -> Result<Self> {
        let axp = Self { device: bus.add_device(address, clock_hz)? };
        axp.read_reg(REG_STATUS1).inspect_err(|err| error!(TAG, "not responding on 0x{address:02x}: {err}"))?;
        Ok(axp)
    }

    /// 500..=3500 mV in 100 mV steps, rounded down.
    pub(crate) fn set_ldo_mv(&self, ldo: Ldo, mv: u32) -> Result<()> {
        let step = (mv.clamp(500, 3500) - 500) / 100;
        self.update_reg(REG_LDO_VOLT0 + ldo.index(), 0x1F, step as u8)
    }

    pub(crate) fn set_ldo_enabled(&self, ldo: Ldo, on: bool) -> Result<()> {
        let bit = 1 << ldo.index();
        self.update_reg(REG_LDO_EN0, bit, if on { bit } else { 0 })
    }

    fn update_reg(&self, reg: u8, mask: u8, value: u8) -> Result<()> {
        let old = self.read_reg(reg)?;
        self.device.transmit(&[reg, (old & !mask) | (value & mask)], TIMEOUT)
    }

    fn read_reg(&self, reg: u8) -> Result<u8> {
        let mut value = [0u8];
        self.device.transmit_receive(&[reg], &mut value, TIMEOUT)?;
        Ok(value[0])
    }
}
