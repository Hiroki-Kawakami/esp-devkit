/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;
use core::time::Duration;

use idf::i2c::{Device, MasterBus};
use idf::log::error;
use idf::{Error, Result};

const TAG: &CStr = c"ina226";

const REG_CONFIG: u8 = 0x00;
const REG_SHUNT_VOLTAGE: u8 = 0x01;
const REG_BUS_VOLTAGE: u8 = 0x02;
const REG_MANUF_ID: u8 = 0xFE;

const MANUF_ID: u16 = 0x5449;
const CONFIG_RESERVED: u16 = 1 << 14;
const CONFIG_AVG_16: u16 = 2 << 9;
const CONFIG_VBUSCT_1100US: u16 = 4 << 6;
const CONFIG_VSHCT_1100US: u16 = 4 << 3;
const CONFIG_MODE_CONTINUOUS: u16 = 7;
const BUS_UV_PER_LSB: u32 = 1250;
const SHUNT_NV_PER_LSB: i32 = 2500;
const I2C_TIMEOUT: Duration = Duration::from_millis(100);

pub(crate) const DEFAULT_HZ: u32 = 400_000;

pub(crate) struct Ina226 {
    device: Device<'static>,
    shunt_mohm: i32,
}

impl Ina226 {
    pub(crate) fn new(bus: &'static MasterBus, address: u16, shunt_mohm: u16) -> Result<Self> {
        let ina = Self { device: bus.add_device(address, DEFAULT_HZ)?, shunt_mohm: i32::from(shunt_mohm) };
        let id = ina.read_reg(REG_MANUF_ID)?;
        if id != MANUF_ID {
            error!(TAG, "unexpected manufacturer id 0x{id:04x}");
            return Err(Error::NOT_FOUND);
        }
        let config = CONFIG_RESERVED | CONFIG_AVG_16 | CONFIG_VBUSCT_1100US | CONFIG_VSHCT_1100US | CONFIG_MODE_CONTINUOUS;
        let [hi, lo] = config.to_be_bytes();
        ina.device.transmit(&[REG_CONFIG, hi, lo], I2C_TIMEOUT)?;
        Ok(ina)
    }

    fn read_reg(&self, reg: u8) -> Result<u16> {
        let mut value = [0u8; 2];
        self.device.transmit_receive(&[reg], &mut value, I2C_TIMEOUT)?;
        Ok(u16::from_be_bytes(value))
    }

    pub(crate) fn bus_voltage_mv(&self) -> Result<u32> {
        Ok(u32::from(self.read_reg(REG_BUS_VOLTAGE)?) * BUS_UV_PER_LSB / 1000)
    }

    /// Positive into the load on the shunt's far side.
    pub(crate) fn current_ma(&self) -> Result<i32> {
        let raw = self.read_reg(REG_SHUNT_VOLTAGE)? as i16;
        Ok(i32::from(raw) * SHUNT_NV_PER_LSB / 1000 / self.shunt_mohm)
    }
}
