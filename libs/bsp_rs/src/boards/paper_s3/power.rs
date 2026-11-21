/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::time::Duration;

use idf::log::warn;
use idf::{Error, Result, gpio, thread};

use super::TAG;
use crate::driver::adc_battery::{self, AdcBattery, Level};
use crate::power::{Battery, Driver, PowerSource, PowerState};

const BATTERY_EMPTY_MV: u32 = 3300;
const BATTERY_FULL_MV: u32 = 4200;

// The main rail over 22K / 22K.
const ADC_VBAT: i32 = 3;
// USB 5 V over 5.1K / 5.1K.
const USB_DET: i32 = 5;
// The charger's CHRG output, open drain and pulled up, low while charging.
const CHG_STAT: i32 = 4;
// A pulse to the power latch cuts the main rail.
const PWROFF_PULSE: i32 = 44;
const PULSE: Duration = Duration::from_millis(50);
const OFF_SETTLE: Duration = Duration::from_millis(500);

// The T5's battery over 100K / 100K; it tells neither USB nor charging.
const T5_BAT_ADC: i32 = 14;

pub(crate) struct PowerDriver {
    battery: Option<AdcBattery>,
    off_pin: Option<i32>,
}

// A part that fails leaves the rest working.
impl PowerDriver {
    pub(super) fn paper_s3() -> Self {
        let off_pin = gpio::configure(&[PWROFF_PULSE], gpio::Mode::Output)
            .and_then(|()| gpio::set_level(PWROFF_PULSE, false))
            .inspect_err(|err| warn!(TAG, "power off unavailable: {err}"))
            .ok()
            .map(|()| PWROFF_PULSE);
        let battery = battery(adc_battery::Config {
            gpio: ADC_VBAT,
            divider: (2, 1),
            empty_mv: BATTERY_EMPTY_MV,
            full_mv: BATTERY_FULL_MV,
            external: Some(Level { gpio: USB_DET, active_high: true }),
            charging: Some(Level { gpio: CHG_STAT, active_high: false }),
        });
        Self { battery, off_pin }
    }

    pub(super) fn t5() -> Self {
        let battery = battery(adc_battery::Config {
            gpio: T5_BAT_ADC,
            divider: (2, 1),
            empty_mv: BATTERY_EMPTY_MV,
            full_mv: BATTERY_FULL_MV,
            external: None,
            charging: None,
        });
        Self { battery, off_pin: None }
    }
}

fn battery(config: adc_battery::Config) -> Option<AdcBattery> {
    AdcBattery::new(config).inspect_err(|err| warn!(TAG, "battery unavailable: {err}")).ok()
}

impl Driver for PowerDriver {
    fn read(&self) -> Result<PowerState> {
        match &self.battery {
            Some(battery) => battery.read(),
            None => Ok(PowerState { source: PowerSource::Unknown, battery: Battery::Unknown }),
        }
    }

    fn off(&self) -> Error {
        let Some(pin) = self.off_pin else { return Error::NOT_SUPPORTED };
        if let Err(err) = gpio::set_level(pin, true) {
            return err;
        }
        thread::sleep(PULSE);
        if let Err(err) = gpio::set_level(pin, false) {
            return err;
        }
        thread::sleep(OFF_SETTLE);
        Error::FAIL
    }
}
