/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;
use core::time::Duration;

use idf::i2c::{Device, MasterBus};
use idf::log::error;
use idf::{Error, Result};

const TAG: &CStr = c"axp2101";

pub(crate) const ADDR: u16 = 0x34;
pub(crate) const DEFAULT_HZ: u32 = 400_000;

const TIMEOUT: Duration = Duration::from_millis(100);

const REG_STATUS1: u8 = 0x00;
const REG_CHIP_ID: u8 = 0x03;
const REG_COMMON_CFG: u8 = 0x10;
const REG_CHARGE_CTRL: u8 = 0x18;
const REG_DCDC_UVP: u8 = 0x23;
const REG_KEY_LEVELS: u8 = 0x27;
const REG_ADC_EN: u8 = 0x30;
const REG_VBAT_H: u8 = 0x34;
const REG_TS_CTRL: u8 = 0x50;
const REG_CHARGE_MV: u8 = 0x64;
const REG_BAT_DET: u8 = 0x68;
const REG_CHGLED: u8 = 0x69;
const REG_LDO_EN0: u8 = 0x90;
const REG_LDO_VOLT0: u8 = 0x92;

const CHIP_ID: u8 = 0x4A;
const STATUS1_VBUS_GOOD: u8 = 1 << 5;
const STATUS1_BATTERY: u8 = 1 << 3;
const STATUS2_DIRECTION: u8 = 0b11 << 5;
const STATUS2_CHARGING: u8 = 0b01 << 5;
const COMMON_CFG_POWER_OFF: u8 = 1 << 0;
const CHARGE_CTRL_CELL: u8 = 1 << 1;
const DCDC_UVP_ALL: u8 = 0x1F;
const TS_CTRL_FIXED_INPUT: u8 = 1 << 4;
const CHGLED_TYPE_A: u8 = 0b00 << 1;
const CHGLED_ENABLE: u8 = 1 << 0;

pub(crate) const ADC_VBAT: u8 = 1 << 0;
pub(crate) const ADC_TS: u8 = 1 << 1;
pub(crate) const ADC_VBUS: u8 = 1 << 2;
pub(crate) const ADC_VSYS: u8 = 1 << 3;
const ADC_ALL: u8 = 0x3F;

pub(crate) struct Status {
    /// External power is up.
    pub vbus: bool,
    pub battery: bool,
    pub charging: bool,
}

#[allow(dead_code)]
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
    /// `NOT_FOUND` when another chip answers at `address`.
    pub(crate) fn new(bus: &'bus MasterBus, address: u16, clock_hz: u32) -> Result<Self> {
        let axp = Self { device: bus.add_device(address, clock_hz)? };
        let id = axp.read_reg(REG_CHIP_ID).inspect_err(|err| error!(TAG, "not responding on 0x{address:02x}: {err}"))?;
        if id != CHIP_ID {
            return Err(Error::NOT_FOUND);
        }
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

    pub(crate) fn status(&self) -> Result<Status> {
        let mut regs = [0u8; 2];
        self.device.transmit_receive(&[REG_STATUS1], &mut regs, TIMEOUT)?;
        Ok(Status {
            vbus: regs[0] & STATUS1_VBUS_GOOD != 0,
            battery: regs[0] & STATUS1_BATTERY != 0,
            charging: regs[1] & STATUS2_DIRECTION == STATUS2_CHARGING,
        })
    }

    /// Meaningless without a battery, where the input floats.
    pub(crate) fn battery_mv(&self) -> Result<u32> {
        // One burst, so the two halves come from the same conversion.
        let mut regs = [0u8; 2];
        self.device.transmit_receive(&[REG_VBAT_H], &mut regs, TIMEOUT)?;
        Ok(u32::from(regs[0] & 0x3F) << 8 | u32::from(regs[1]))
    }

    pub(crate) fn set_charge_enabled(&self, on: bool) -> Result<()> {
        self.update_reg(REG_CHARGE_CTRL, CHARGE_CTRL_CELL, if on { CHARGE_CTRL_CELL } else { 0 })
    }

    /// One of 4000, 4100, 4200, 4350 and 4400.
    pub(crate) fn set_charge_mv(&self, mv: u32) -> Result<()> {
        let code = match mv {
            4000 => 1,
            4100 => 2,
            4200 => 3,
            4350 => 4,
            4400 => 5,
            _ => return Err(Error::INVALID_ARG),
        };
        self.update_reg(REG_CHARGE_MV, 0x07, code)
    }

    /// With `false` the TS pin is a plain input, for a board that wires
    /// something other than a battery thermistor to it.
    pub(crate) fn set_ts_thermistor(&self, on: bool) -> Result<()> {
        self.update_reg(REG_TS_CTRL, TS_CTRL_FIXED_INPUT, if on { 0 } else { TS_CTRL_FIXED_INPUT })
    }

    /// `ADC_*` bits.
    pub(crate) fn set_adc_channels(&self, channels: u8) -> Result<()> {
        self.update_reg(REG_ADC_EN, ADC_ALL, channels)
    }

    pub(crate) fn set_battery_detection(&self, on: bool) -> Result<()> {
        self.update_reg(REG_BAT_DET, 0x01, u8::from(on))
    }

    /// Whether a DCDC falling under 85% of its setting powers everything off.
    pub(crate) fn set_dcdc_undervoltage_off(&self, on: bool) -> Result<()> {
        self.update_reg(REG_DCDC_UVP, DCDC_UVP_ALL, if on { DCDC_UVP_ALL } else { 0 })
    }

    /// How long the power key is held for an interrupt (1000, 1500, 2000 or
    /// 2500 ms), to force the power off (4000, 6000, 8000 or 10000 ms) and to
    /// power on (128, 512, 1000 or 2000 ms).
    pub(crate) fn set_power_key_ms(&self, irq: u32, off: u32, on: u32) -> Result<()> {
        let code = |ms, steps: [u32; 4]| steps.iter().position(|&s| s == ms).map(|i| i as u8).ok_or(Error::INVALID_ARG);
        let irq = code(irq, [1000, 1500, 2000, 2500])?;
        let off = code(off, [4000, 6000, 8000, 10000])?;
        let on = code(on, [128, 512, 1000, 2000])?;
        self.update_reg(REG_KEY_LEVELS, 0x3F, irq << 4 | off << 2 | on)
    }

    /// Lets the charger drive the charge LED pin in its type A pattern.
    pub(crate) fn enable_charge_led(&self) -> Result<()> {
        self.update_reg(REG_CHGLED, 0x07, CHGLED_TYPE_A | CHGLED_ENABLE)
    }

    /// Cuts every rail but the RTC's.
    pub(crate) fn power_off(&self) -> Result<()> {
        self.update_reg(REG_COMMON_CFG, COMMON_CFG_POWER_OFF, COMMON_CFG_POWER_OFF)
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
