/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::time::Duration;

use idf::{Error, Result, thread};

use super::device::Pmu;
use crate::devices::axp192::Rail;
use crate::devices::axp2101::{self, Ldo};
use crate::power::{self, Battery, BatteryState, Charge, Driver, PowerSource, PowerState};

const BATTERY_EMPTY_MV: u32 = 3300;
const BATTERY_FULL_MV: u32 = 4200;
const CHARGE_MV: u32 = 4200;
const CHARGE_MA: u32 = 390;
const OFF_SETTLE: Duration = Duration::from_millis(500);

// M5Unified's power-up values for the boards with this chip. DCDC1 feeds the
// ESP32; the output enables in 0x12 stay as they are, since rewriting them
// cuts the bus output for a moment after a reset.
const AXP192_POWER_UP: [(u8, u8); 21] = [
    (0x26, 0x6A), // DCDC1 3350 mV
    (0x30, 0b0000_0010), // VBUS path, 500 mA limit
    (0x31, 0b0000_0100), // VOFF 3.0 V
    (0x32, 0b0100_0010), // battery detection
    (0x33, 0b1100_0000), // charge on, 4.2 V
    (0x35, 0xA2), // backup battery charge
    (0x36, 0x0C), // power key: 128 ms on, 4 s off
    (0x40, 0x00),
    (0x41, 0x00),
    (0x42, 0x03), // power key IRQs only
    (0x43, 0x00),
    (0x44, 0x00),
    (0x82, 0xFF), // ADCs on
    (0x83, 0x80),
    (0x84, 0x32), // ADC 25 Hz
    (0x90, 0x07), // GPIO0 floating
    (0x91, 0xA0),
    (0x98, 0x00), // PWM1, the power LED, off
    (0x99, 0xFF),
    (0x9A, 0xFF),
    (0x92, 0x02), // GPIO1 from PWM1
];

/// Sets the charger, the ADCs and the power key up, once, and stops the
/// vibration motor.
pub(super) fn configure(pmu: &Pmu) -> Result<()> {
    match pmu {
        Pmu::Axp192(axp) => {
            axp.write_regs(&AXP192_POWER_UP)?;
            axp.set_rail_enabled(Rail::Ldo3, false)?;
            axp.set_charge_ma(CHARGE_MA)
        }
        Pmu::Axp2101(axp) => {
            axp.set_power_key_ms(1000, 4000, 128)?;
            axp.set_battery_detection(true)?;
            axp.enable_charge_led()?;
            axp.set_ldo_mv(Ldo::Dldo1, 500)?;
            axp.set_adc_channels(axp2101::ADC_VBAT | axp2101::ADC_TS | axp2101::ADC_VBUS | axp2101::ADC_VSYS)?;
            // A load step dips a DCDC under its threshold and would power the board off.
            axp.set_dcdc_undervoltage_off(false)?;
            axp.set_charge_mv(CHARGE_MV)?;
            // TS carries no thermistor here; read as one, it can hold the charger off.
            axp.set_ts_thermistor(false)
        }
    }
}

pub(crate) struct PowerDriver {
    pmu: &'static Pmu,
}

impl PowerDriver {
    pub(super) fn new(pmu: &'static Pmu) -> Self {
        Self { pmu }
    }
}

fn battery(present: bool, charging: bool, mv: impl FnOnce() -> Result<u32>) -> Result<Battery> {
    if !present {
        return Ok(Battery::Absent);
    }
    let mv = mv()?;
    Ok(Battery::Present(BatteryState {
        level: power::linear_level(mv, BATTERY_EMPTY_MV, BATTERY_FULL_MV),
        voltage_mv: Some(mv),
        charging: Some(charging),
    }))
}

impl Driver for PowerDriver {
    fn read(&self) -> Result<PowerState> {
        let (vbus, battery) = match self.pmu {
            Pmu::Axp192(axp) => {
                let status = axp.status()?;
                (status.external, battery(status.battery, status.charging, || axp.battery_mv())?)
            }
            Pmu::Axp2101(axp) => {
                let status = axp.status()?;
                (status.vbus, battery(status.battery, status.charging, || axp.battery_mv())?)
            }
        };
        let source = if vbus { PowerSource::External } else { PowerSource::Battery };
        Ok(PowerState { source, battery })
    }

    fn set_charge(&self, charge: Charge) -> Result<()> {
        let on = match charge {
            Charge::Off => false,
            Charge::Normal => true,
            Charge::Fast => return Err(Error::NOT_SUPPORTED),
        };
        match self.pmu {
            Pmu::Axp192(axp) => axp.set_charge_enabled(on),
            Pmu::Axp2101(axp) => axp.set_charge_enabled(on),
        }
    }

    fn off(&self) -> Error {
        let result = match self.pmu {
            Pmu::Axp192(axp) => axp.power_off(),
            Pmu::Axp2101(axp) => axp.power_off(),
        };
        if let Err(err) = result {
            return err;
        }
        thread::sleep(OFF_SETTLE);
        Error::FAIL
    }
}
