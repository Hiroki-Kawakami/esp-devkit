/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::time::Duration;

use idf::sync::Mutex;
use idf::{Error, Result, thread};

use crate::devices::aw9523::Aw9523;
use crate::devices::axp2101::{self, Axp2101};
use crate::power::{self, Battery, BatteryState, Charge, Driver, PowerSource, PowerState, Switch};

pub(super) const AW9523_USB_OTG_EN: u8 = 5;
pub(super) const AW9523_BOOST_EN: u8 = 15;

const CHARGE_MV: u32 = 4200;
// The fuel gauge needs a model of the cell, which the CoreS3's is not published
// as; without one it read 87% from a cell about to cut out.
const BATTERY_EMPTY_MV: u32 = 3300;
const BATTERY_FULL_MV: u32 = 4200;
const OFF_SETTLE: Duration = Duration::from_millis(500);

/// Sets the charger and the power key up, once.
pub(super) fn configure(axp: &Axp2101) -> Result<()> {
    // TS carries the 5 V bus rather than a thermistor; read as one, it is a
    // battery too cold to charge, and the charger never starts.
    axp.set_ts_thermistor(false)?;
    axp.set_adc_channels(axp2101::ADC_VBAT | axp2101::ADC_TS | axp2101::ADC_VBUS | axp2101::ADC_VSYS)?;
    axp.set_battery_detection(true)?;
    axp.set_charge_mv(CHARGE_MV)?;
    // A load step dips a DCDC under its threshold and would power the board off.
    axp.set_dcdc_undervoltage_off(false)?;
    axp.set_power_key_ms(1000, 4000, 128)?;
    axp.enable_charge_led()
}

pub(crate) struct PowerDriver {
    axp: &'static Axp2101<'static>,
    io: &'static Mutex<Aw9523<'static>>,
}

impl PowerDriver {
    pub(super) fn new(axp: &'static Axp2101<'static>, io: &'static Mutex<Aw9523<'static>>) -> Self {
        Self { axp, io }
    }
}

impl Driver for PowerDriver {
    fn read(&self) -> Result<PowerState> {
        let status = self.axp.status()?;
        let source = if status.vbus { PowerSource::External } else { PowerSource::Battery };
        let battery = if status.battery {
            let mv = self.axp.battery_mv()?;
            Battery::Present(BatteryState {
                level: power::linear_level(mv, BATTERY_EMPTY_MV, BATTERY_FULL_MV),
                voltage_mv: Some(mv),
                charging: Some(status.charging),
            })
        } else {
            Battery::Absent
        };
        Ok(PowerState { source, battery })
    }

    fn set_charge(&self, charge: Charge) -> Result<()> {
        match charge {
            Charge::Off => self.axp.set_charge_enabled(false),
            Charge::Normal => self.axp.set_charge_enabled(true),
            Charge::Fast => Err(Error::NOT_SUPPORTED),
        }
    }

    // The USB port's 5 V comes from the boost converter through the OTG switch.
    fn set_switch(&self, switch: Switch, on: bool) -> Result<()> {
        match switch {
            Switch::Usb5V => {
                let mut io = self.io.lock();
                if on {
                    io.set_output(AW9523_BOOST_EN, true)?;
                    io.set_output(AW9523_USB_OTG_EN, true)
                } else {
                    io.set_output(AW9523_USB_OTG_EN, false)?;
                    io.set_output(AW9523_BOOST_EN, false)
                }
            }
        }
    }

    fn off(&self) -> Error {
        if let Err(err) = self.axp.power_off() {
            return err;
        }
        thread::sleep(OFF_SETTLE);
        Error::FAIL
    }
}
