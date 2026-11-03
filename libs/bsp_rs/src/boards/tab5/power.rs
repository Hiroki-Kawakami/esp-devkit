/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::time::Duration;

use idf::i2c::MasterBus;
use idf::log::warn;
use idf::sync::Mutex;
use idf::{Error, Result, thread};

use super::TAG;
use crate::devices::ina226::Ina226;
use crate::devices::pi4io::Pi4io;
use crate::power::{self, BatteryState, Charge, Driver, PowerSource, PowerState, Switch};

const INA226_ADDR: u16 = 0x41;
const INA226_SHUNT_MOHM: u16 = 5;

pub(super) const USB5V_EN_PIN: u8 = 3;
pub(super) const PWROFF_PIN: u8 = 4;
pub(super) const CHG_QC_EN_N_PIN: u8 = 5;
pub(super) const CHG_EN_PIN: u8 = 7;

const PWROFF_PULSE: Duration = Duration::from_millis(500);

// Charge current reads negative. CHG_STAT reads high in every state without a
// pull and nothing reports the external supply, so the current decides both.
const CHARGING_MAX_MA: i32 = -50;
const DISCHARGE_MIN_MA: i32 = 10;

const BATTERY_EMPTY_MV: u32 = 6600;
const BATTERY_FULL_MV: u32 = 8200;

pub(crate) struct PowerDriver {
    ina: Option<Ina226>,
    io: &'static Mutex<Pi4io<'static>>,
}

impl PowerDriver {
    // Without the sensor the board can still switch off.
    pub(super) fn new(bus: &'static MasterBus, io: &'static Mutex<Pi4io<'static>>) -> Self {
        let ina = Ina226::new(bus, INA226_ADDR, INA226_SHUNT_MOHM)
            .inspect_err(|err| warn!(TAG, "battery sensing unavailable: {err}"))
            .ok();
        Self { ina, io }
    }
}

impl Driver for PowerDriver {
    fn read(&self) -> Result<PowerState> {
        let ina = self.ina.as_ref().ok_or(Error::INVALID_STATE)?;
        let mv = ina.bus_voltage_mv()?;
        let ma = ina.current_ma()?;
        let source = if ma < DISCHARGE_MIN_MA { PowerSource::External } else { PowerSource::Battery };
        Ok(PowerState {
            source: Some(source),
            battery: Some(BatteryState {
                level: power::linear_level(mv, BATTERY_EMPTY_MV, BATTERY_FULL_MV),
                voltage_mv: Some(mv),
                charging: Some(ma <= CHARGING_MAX_MA),
            }),
        })
    }

    fn set_charge(&self, charge: Charge) -> Result<()> {
        let mut io = self.io.lock();
        io.set_output(CHG_QC_EN_N_PIN, charge != Charge::Fast)?;
        io.set_output(CHG_EN_PIN, charge != Charge::Off)
    }

    fn set_switch(&self, switch: Switch, on: bool) -> Result<()> {
        match switch {
            Switch::Usb5V => self.io.lock().set_output(USB5V_EN_PIN, on),
        }
    }

    fn off(&self) -> Error {
        let pulse = |high| self.io.lock().set_output(PWROFF_PIN, high);
        if let Err(err) = pulse(true) {
            return err;
        }
        thread::sleep(PWROFF_PULSE);
        pulse(false).err().unwrap_or(Error::FAIL)
    }
}
