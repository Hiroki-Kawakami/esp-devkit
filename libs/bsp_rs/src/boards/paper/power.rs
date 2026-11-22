/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::time::Duration;

use idf::log::warn;
use idf::{Error, Result, gpio, thread};

use super::TAG;
use crate::driver::adc_battery::{self, AdcBattery};
use crate::power::{Battery, Driver, PowerSource, PowerState};

const BATTERY_EMPTY_MV: u32 = 3300;
const BATTERY_FULL_MV: u32 = 4200;

// The battery over a 1:1 divider; nothing tells USB or charging.
const ADC_VBAT: i32 = 35;
const OFF_SETTLE: Duration = Duration::from_millis(500);

pub(crate) struct PowerDriver {
    battery: Option<AdcBattery>,
    /// Holds the main rail up while high.
    hold: i32,
}

impl PowerDriver {
    pub(super) fn new(hold: i32) -> Self {
        let battery = AdcBattery::new(adc_battery::Config {
            gpio: ADC_VBAT,
            divider: (2, 1),
            empty_mv: BATTERY_EMPTY_MV,
            full_mv: BATTERY_FULL_MV,
            external: None,
            charging: None,
        })
        .inspect_err(|err| warn!(TAG, "battery unavailable: {err}"))
        .ok();
        Self { battery, hold }
    }
}

impl Driver for PowerDriver {
    fn read(&self) -> Result<PowerState> {
        match &self.battery {
            Some(battery) => battery.read(),
            None => Ok(PowerState { source: PowerSource::Unknown, battery: Battery::Unknown }),
        }
    }

    // USB, the power button held down or the RTC's alarm keep the rail up
    // past the release; the hold is then taken back.
    fn off(&self) -> Error {
        if let Err(err) = gpio::set_level(self.hold, false) {
            return err;
        }
        thread::sleep(OFF_SETTLE);
        if let Err(err) = gpio::set_level(self.hold, true) {
            return err;
        }
        Error::FAIL
    }
}
