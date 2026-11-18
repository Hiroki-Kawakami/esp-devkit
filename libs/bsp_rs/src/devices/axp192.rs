/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::time::Duration;

use idf::i2c::{Device, MasterBus};
use idf::{Error, Result};

pub(crate) const ADDR: u16 = 0x34;
pub(crate) const DEFAULT_HZ: u32 = 400_000;

const TIMEOUT: Duration = Duration::from_millis(100);

const REG_STATUS: u8 = 0x00;
const REG_MODE: u8 = 0x01;
const REG_CHIP_ID: u8 = 0x03;
const REG_OUTPUT: u8 = 0x12;
const REG_SHUTDOWN: u8 = 0x32;
const REG_CHARGE1: u8 = 0x33;
const REG_VBAT_H: u8 = 0x78;
const REG_DCDC1_VOLT: u8 = 0x26;
const REG_DCDC3_VOLT: u8 = 0x27;
const REG_LDO23_VOLT: u8 = 0x28;
const REG_GPIO2_FUNC: u8 = 0x93;
const REG_GPIO34_FUNC: u8 = 0x95;
const REG_GPIO34_LEVEL: u8 = 0x96;

const CHIP_ID: u8 = 0x03;
// GPIO3 and GPIO4 as GPIOs, GPIO4 an NMOS open-drain output.
const GPIO34_ENABLE: u8 = 1 << 7;
const GPIO4_FUNC_MASK: u8 = 0b11 << 2;
const GPIO4_NMOS_OUTPUT: u8 = 0b01 << 2;
const GPIO4_LEVEL: u8 = 1 << 1;
const GPIO2_FLOATING: u8 = 0x06;
const GPIO2_LOW: u8 = 0x05;
const STATUS_ACIN: u8 = 1 << 7;
const STATUS_VBUS: u8 = 1 << 5;
const STATUS_CHARGING: u8 = 1 << 2;
const MODE_BATTERY: u8 = 1 << 5;
const SHUTDOWN_OFF: u8 = 1 << 7;
const CHARGE1_ENABLE: u8 = 1 << 7;
const CHARGE_STEPS_MA: [u32; 16] = [100, 190, 280, 360, 450, 550, 630, 700, 780, 880, 960, 1080, 1160, 1240, 1320, 1360];

pub(crate) struct Status {
    /// Either input, ACIN or VBUS, powered.
    pub external: bool,
    pub battery: bool,
    pub charging: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Rail {
    Dcdc1,
    Dcdc3,
    Ldo2,
    Ldo3,
}

impl Rail {
    const fn bit(self) -> u8 {
        match self {
            Self::Dcdc1 => 1 << 0,
            Self::Dcdc3 => 1 << 1,
            Self::Ldo2 => 1 << 2,
            Self::Ldo3 => 1 << 3,
        }
    }
}

pub(crate) struct Axp192<'bus> {
    device: Device<'bus>,
}

impl<'bus> Axp192<'bus> {
    /// `NOT_FOUND` when another chip answers at `address`.
    pub(crate) fn new(bus: &'bus MasterBus, address: u16, clock_hz: u32) -> Result<Self> {
        let axp = Self { device: bus.add_device(address, clock_hz)? };
        if axp.read_reg(REG_CHIP_ID)? != CHIP_ID {
            return Err(Error::NOT_FOUND);
        }
        Ok(axp)
    }

    pub(crate) fn set_rail_enabled(&self, rail: Rail, on: bool) -> Result<()> {
        self.update_reg(REG_OUTPUT, rail.bit(), if on { rail.bit() } else { 0 })
    }

    /// DCDC1 and DCDC3: 700..=3500 mV in 25 mV steps, rounded down. LDO2 and
    /// LDO3: 1800..=3300 mV in 100 mV steps.
    pub(crate) fn set_rail_mv(&self, rail: Rail, mv: u32) -> Result<()> {
        match rail {
            Rail::Dcdc1 | Rail::Dcdc3 => {
                let reg = if rail == Rail::Dcdc1 { REG_DCDC1_VOLT } else { REG_DCDC3_VOLT };
                self.update_reg(reg, 0x7F, ((mv.clamp(700, 3500) - 700) / 25) as u8)
            }
            Rail::Ldo2 | Rail::Ldo3 => {
                let step = ((mv.clamp(1800, 3300) - 1800) / 100) as u8;
                if rail == Rail::Ldo2 { self.update_reg(REG_LDO23_VOLT, 0xF0, step << 4) } else { self.update_reg(REG_LDO23_VOLT, 0x0F, step) }
            }
        }
    }

    /// Makes GPIO4 an open-drain output: `true` lets it float, `false` pulls
    /// it low.
    pub(crate) fn set_gpio4(&self, high: bool) -> Result<()> {
        self.update_reg(REG_GPIO34_FUNC, GPIO34_ENABLE | GPIO4_FUNC_MASK, GPIO34_ENABLE | GPIO4_NMOS_OUTPUT)?;
        self.update_reg(REG_GPIO34_LEVEL, GPIO4_LEVEL, if high { GPIO4_LEVEL } else { 0 })
    }

    /// GPIO2 floating with `true`, pulled low with `false`.
    pub(crate) fn set_gpio2(&self, high: bool) -> Result<()> {
        self.write_reg(REG_GPIO2_FUNC, if high { GPIO2_FLOATING } else { GPIO2_LOW })
    }

    pub(crate) fn status(&self) -> Result<Status> {
        let (status, mode) = (self.read_reg(REG_STATUS)?, self.read_reg(REG_MODE)?);
        Ok(Status { external: status & (STATUS_ACIN | STATUS_VBUS) != 0, battery: mode & MODE_BATTERY != 0, charging: status & STATUS_CHARGING != 0 })
    }

    /// The battery's voltage, from the ADC, which must be on.
    pub(crate) fn battery_mv(&self) -> Result<u32> {
        let mut raw = [0u8; 2];
        self.device.transmit_receive(&[REG_VBAT_H], &mut raw, TIMEOUT)?;
        let code = u32::from(raw[0]) << 4 | u32::from(raw[1] & 0x0F);
        Ok(code * 11 / 10)
    }

    pub(crate) fn set_charge_enabled(&self, on: bool) -> Result<()> {
        self.update_reg(REG_CHARGE1, CHARGE1_ENABLE, if on { CHARGE1_ENABLE } else { 0 })
    }

    /// The largest step not above `ma`, and 100 mA below that.
    pub(crate) fn set_charge_ma(&self, ma: u32) -> Result<()> {
        let step = CHARGE_STEPS_MA.iter().rposition(|&s| s <= ma).unwrap_or(0);
        self.update_reg(REG_CHARGE1, 0x0F, step as u8)
    }

    /// Cuts every output.
    pub(crate) fn power_off(&self) -> Result<()> {
        self.update_reg(REG_SHUTDOWN, SHUTDOWN_OFF, SHUTDOWN_OFF)
    }

    /// Writes `regs` in order, for a board's power-up values of registers
    /// this driver has no setter for.
    pub(crate) fn write_regs(&self, regs: &[(u8, u8)]) -> Result<()> {
        regs.iter().try_for_each(|&(reg, value)| self.write_reg(reg, value))
    }

    fn write_reg(&self, reg: u8, value: u8) -> Result<()> {
        self.device.transmit(&[reg, value], TIMEOUT)
    }

    fn update_reg(&self, reg: u8, mask: u8, value: u8) -> Result<()> {
        let old = self.read_reg(reg)?;
        self.write_reg(reg, (old & !mask) | (value & mask))
    }

    fn read_reg(&self, reg: u8) -> Result<u8> {
        let mut value = [0u8];
        self.device.transmit_receive(&[reg], &mut value, TIMEOUT)?;
        Ok(value[0])
    }
}
