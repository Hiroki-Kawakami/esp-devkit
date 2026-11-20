/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::time::Duration;

use idf::sync::Mutex;
use idf::{Error, Result, thread};

use crate::devices::m5pm1::M5pm1;
use crate::power::{self, Battery, BatteryState, Charge, Driver, PowerSource, PowerState};

// The charger's CHRG output, open drain, low while charging.
const PM1_CHG_STAT: u8 = 2;
// Low sets the charger's fast current; left floating, its normal one.
const PM1_CHG_PROG: u8 = 3;

const BATTERY_EMPTY_MV: u32 = 3300;
const BATTERY_FULL_MV: u32 = 4200;
const OFF_SETTLE: Duration = Duration::from_millis(500);

pub(crate) struct PowerDriver {
    pm1: &'static Mutex<M5pm1<'static>>,
}

impl PowerDriver {
    pub(super) fn new(pm1: &'static Mutex<M5pm1<'static>>) -> Result<Self> {
        pm1.lock().set_input(PM1_CHG_STAT, true)?;
        let driver = Self { pm1 };
        driver.set_charge(Charge::Fast)?;
        Ok(driver)
    }
}

impl Driver for PowerDriver {
    fn read(&self) -> Result<PowerState> {
        let pm1 = self.pm1.lock();
        let sources = pm1.sources()?;
        let external = sources.vin || sources.vinout;
        let source = if external { PowerSource::External } else { PowerSource::Battery };
        let battery = if sources.battery {
            let mv = pm1.battery_mv()?;
            let charging = external && !pm1.level(PM1_CHG_STAT)?;
            Battery::Present(BatteryState {
                level: power::linear_level(mv, BATTERY_EMPTY_MV, BATTERY_FULL_MV),
                voltage_mv: Some(mv),
                charging: Some(charging),
            })
        } else {
            Battery::Absent
        };
        Ok(PowerState { source, battery })
    }

    fn set_charge(&self, charge: Charge) -> Result<()> {
        let pm1 = self.pm1.lock();
        match charge {
            Charge::Off => return pm1.set_charge_enabled(false),
            Charge::Normal => pm1.set_input(PM1_CHG_PROG, false)?,
            Charge::Fast => pm1.set_output(PM1_CHG_PROG, false)?,
        }
        pm1.set_charge_enabled(true)
    }

    fn off(&self) -> Error {
        if let Err(err) = self.pm1.lock().power_off() {
            return err;
        }
        thread::sleep(OFF_SETTLE);
        Error::FAIL
    }
}
