/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;
use core::time::Duration;

use idf::i2c::{Device, MasterBus};
use idf::log::error;
use idf::{Error, Result, thread};

use super::bmi270_config::CONFIG_FILE;
use crate::imu::{Driver, Reading, Settings, Vec3};

const TAG: &CStr = c"bmi270";

const REG_CHIP_ID: u8 = 0x00;
const REG_STATUS: u8 = 0x03;
const REG_ACC_X_LSB: u8 = 0x0C;
const REG_GYR_X_LSB: u8 = 0x12;
const REG_INTERNAL_STATUS: u8 = 0x21;
const REG_ACC_CONF: u8 = 0x40;
const REG_ACC_RANGE: u8 = 0x41;
const REG_GYR_CONF: u8 = 0x42;
const REG_GYR_RANGE: u8 = 0x43;
const REG_INIT_CTRL: u8 = 0x59;
const REG_INIT_ADDR_0: u8 = 0x5B;
const REG_INIT_DATA: u8 = 0x5E;
const REG_PWR_CONF: u8 = 0x7C;
const REG_PWR_CTRL: u8 = 0x7D;
const REG_CMD: u8 = 0x7E;

const CHIP_ID: u8 = 0x24;
const CMD_SOFT_RESET: u8 = 0xB6;
const STATUS_DRDY_ACC: u8 = 0x80;
const STATUS_DRDY_GYR: u8 = 0x40;
const INTERNAL_STATUS_MASK: u8 = 0x0F;
const INTERNAL_STATUS_OK: u8 = 0x01;
const PWR_CONF_FUP_EN: u8 = 0x02;
const PWR_CTRL_GYR_EN: u8 = 0x02;
const PWR_CTRL_ACC_EN: u8 = 0x04;
const CONF_FILTER_PERF: u8 = 0x80;
const CONF_BWP_NORMAL: u8 = 0x20;
const ODR_CODE_25HZ: u8 = 0x06;
const DATA_BURST_LEN: usize = (REG_GYR_X_LSB + 6 - REG_STATUS) as usize;
const FULL_SCALE: f32 = 32768.0;

const CONFIG_CHUNK: usize = 256;
const I2C_TIMEOUT: Duration = Duration::from_millis(100);
const INIT_POLL: Duration = Duration::from_millis(10);
const INIT_POLLS: usize = 20;
const RESET_WAIT: Duration = Duration::from_millis(2);
const POWER_SAVE_WAIT: Duration = Duration::from_millis(1);

const ODR_HZ: [u16; 7] = [25, 50, 100, 200, 400, 800, 1600];
const ACCEL_RANGE_G: [u16; 4] = [2, 4, 8, 16];
const ACCEL_RANGE_CODE: [u8; 4] = [0x00, 0x01, 0x02, 0x03];
const GYRO_RANGE_DPS: [u16; 5] = [125, 250, 500, 1000, 2000];
const GYRO_RANGE_CODE: [u8; 5] = [0x04, 0x03, 0x02, 0x01, 0x00];

pub(crate) const ADDR: u16 = 0x68;
pub(crate) const DEFAULT_HZ: u32 = 400_000;

const INITIAL: Settings = Settings { odr_hz: 100, accel_range_g: 8, gyro_range_dps: 2000 };

fn pick_at_least(table: &[u16], value: u16) -> usize {
    table.iter().position(|&v| v >= value).unwrap_or(table.len() - 1)
}

fn vec3(bytes: &[u8], scale: f32) -> Vec3 {
    let axis = |i: usize| f32::from(i16::from_le_bytes([bytes[i], bytes[i + 1]])) * scale;
    Vec3 { x: axis(0), y: axis(2), z: axis(4) }
}

pub(crate) struct Bmi270 {
    device: Device<'static>,
    settings: Settings,
    accel_scale: f32,
    gyro_scale: f32,
    accel: Vec3,
    gyro: Vec3,
}

impl Bmi270 {
    pub(crate) fn new(bus: &'static MasterBus, address: u16, clock_hz: u32) -> Result<Self> {
        let device = bus.add_device(address, clock_hz)?;
        let mut chip = Self {
            device,
            settings: INITIAL,
            accel_scale: 0.0,
            gyro_scale: 0.0,
            accel: Vec3::default(),
            gyro: Vec3::default(),
        };
        chip.init()?;
        chip.configure(&INITIAL)?;
        chip.enable()?;
        chip.wait_first_accel()?;
        Ok(chip)
    }

    fn write_reg(&self, reg: u8, value: u8) -> Result<()> {
        self.device.transmit(&[reg, value], I2C_TIMEOUT)
    }

    fn read_regs(&self, reg: u8, data: &mut [u8]) -> Result<()> {
        self.device.transmit_receive(&[reg], data, I2C_TIMEOUT)
    }

    fn read_reg(&self, reg: u8) -> Result<u8> {
        let mut value = [0u8];
        self.read_regs(reg, &mut value)?;
        Ok(value[0])
    }

    fn upload_config(&self) -> Result<()> {
        let mut buf = [0u8; 1 + CONFIG_CHUNK];
        buf[0] = REG_INIT_DATA;
        for (i, chunk) in CONFIG_FILE.chunks(CONFIG_CHUNK).enumerate() {
            let index = i * CONFIG_CHUNK;
            let addr = [REG_INIT_ADDR_0, ((index >> 1) & 0x0F) as u8, (index >> 5) as u8];
            self.device.transmit(&addr, I2C_TIMEOUT)?;
            buf[1..=chunk.len()].copy_from_slice(chunk);
            self.device.transmit(&buf[..=chunk.len()], I2C_TIMEOUT)?;
        }
        Ok(())
    }

    fn init(&self) -> Result<()> {
        let id = self.read_reg(REG_CHIP_ID)?;
        if id != CHIP_ID {
            error!(TAG, "unexpected chip id 0x{id:02x}");
            return Err(Error::NOT_FOUND);
        }
        self.write_reg(REG_CMD, CMD_SOFT_RESET)?;
        thread::sleep(RESET_WAIT);
        self.write_reg(REG_PWR_CONF, 0x00)?;
        thread::sleep(POWER_SAVE_WAIT);
        self.write_reg(REG_INIT_CTRL, 0x00)?;
        self.upload_config()?;
        self.write_reg(REG_INIT_CTRL, 0x01)?;

        let mut status = 0;
        for _ in 0..INIT_POLLS {
            thread::sleep(INIT_POLL);
            if let Ok(value) = self.read_reg(REG_INTERNAL_STATUS) {
                status = value;
                if status & INTERNAL_STATUS_MASK == INTERNAL_STATUS_OK {
                    return Ok(());
                }
            }
        }
        error!(TAG, "config load failed (internal status 0x{status:02x})");
        Err(Error::FAIL)
    }

    fn enable(&self) -> Result<()> {
        self.write_reg(REG_PWR_CTRL, PWR_CTRL_ACC_EN | PWR_CTRL_GYR_EN)?;
        self.write_reg(REG_PWR_CONF, PWR_CONF_FUP_EN)
    }

    fn apply(&mut self, odr: usize, accel: usize, gyro: usize) -> Result<()> {
        let odr_code = ODR_CODE_25HZ + odr as u8;
        self.write_reg(REG_ACC_CONF, CONF_FILTER_PERF | CONF_BWP_NORMAL | odr_code)?;
        self.write_reg(REG_ACC_RANGE, ACCEL_RANGE_CODE[accel])?;
        self.accel_scale = f32::from(ACCEL_RANGE_G[accel]) / FULL_SCALE;
        self.write_reg(REG_GYR_CONF, CONF_FILTER_PERF | CONF_BWP_NORMAL | odr_code)?;
        self.write_reg(REG_GYR_RANGE, GYRO_RANGE_CODE[gyro])?;
        self.gyro_scale = f32::from(GYRO_RANGE_DPS[gyro]) / FULL_SCALE;

        let mut discard = [0u8; DATA_BURST_LEN];
        self.read_regs(REG_STATUS, &mut discard)
    }

    fn fetch(&mut self) -> Result<u8> {
        let mut buf = [0u8; DATA_BURST_LEN];
        self.read_regs(REG_STATUS, &mut buf)?;
        let status = buf[0];
        if status & STATUS_DRDY_ACC != 0 {
            self.accel = vec3(&buf[usize::from(REG_ACC_X_LSB - REG_STATUS)..], self.accel_scale);
        }
        if status & STATUS_DRDY_GYR != 0 {
            self.gyro = vec3(&buf[usize::from(REG_GYR_X_LSB - REG_STATUS)..], self.gyro_scale);
        }
        Ok(status)
    }

    fn wait_first_accel(&mut self) -> Result<()> {
        for _ in 0..INIT_POLLS {
            thread::sleep(INIT_POLL);
            if self.fetch()? & STATUS_DRDY_ACC != 0 {
                return Ok(());
            }
        }
        Err(Error::TIMEOUT)
    }
}

impl Driver for Bmi270 {
    fn settings(&self) -> Settings {
        self.settings
    }

    fn read(&mut self) -> Result<Reading> {
        let status = self.fetch()?;
        Ok(Reading {
            accel: self.accel,
            gyro: self.gyro,
            updated: status & (STATUS_DRDY_ACC | STATUS_DRDY_GYR) != 0,
        })
    }

    fn configure(&mut self, settings: &Settings) -> Result<Settings> {
        let odr = pick_at_least(&ODR_HZ, settings.odr_hz);
        let accel = pick_at_least(&ACCEL_RANGE_G, settings.accel_range_g);
        let gyro = pick_at_least(&GYRO_RANGE_DPS, settings.gyro_range_dps);

        // A running sensor keeps raising drdy with data at the old range for up
        // to about 300 ms after a change, so it is paused around it.
        let pwr_ctrl = self.read_reg(REG_PWR_CTRL)?;
        let sensors = pwr_ctrl & (PWR_CTRL_ACC_EN | PWR_CTRL_GYR_EN);
        if sensors != 0 {
            self.write_reg(REG_PWR_CTRL, pwr_ctrl & !sensors)?;
        }
        let applied = self.apply(odr, accel, gyro);
        if sensors != 0 {
            let restored = self.write_reg(REG_PWR_CTRL, pwr_ctrl);
            applied.and(restored)?;
        } else {
            applied?;
        }

        self.settings = Settings {
            odr_hz: ODR_HZ[odr],
            accel_range_g: ACCEL_RANGE_G[accel],
            gyro_range_dps: GYRO_RANGE_DPS[gyro],
        };
        Ok(self.settings)
    }
}
