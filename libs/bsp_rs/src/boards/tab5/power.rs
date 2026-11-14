/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::time::Duration;

use idf::i2c::MasterBus;
use idf::log::warn;
use idf::sync::Mutex;
use idf::time::Instant;
use idf::{Error, Result, thread};

use super::TAG;
use crate::devices::ina226::Ina226;
use crate::devices::pi4io::Pi4io;
use crate::dispatch::{self, Next, Source};
use crate::power::{self, Battery, BatteryState, Charge, Driver, PowerSource, PowerState, Switch};

const INA226_ADDR: u16 = 0x41;
const INA226_SHUNT_MOHM: u16 = 5;

pub(super) const USB5V_EN_PIN: u8 = 3;
pub(super) const PWROFF_PIN: u8 = 4;
pub(super) const CHG_QC_EN_N_PIN: u8 = 5;
pub(super) const CHG_EN_PIN: u8 = 7;

const PWROFF_PULSE: Duration = Duration::from_millis(500);

// Charge current reads negative. CHG_STAT reads high in every state even with a
// pull-down and nothing reports the external supply, so the current decides both.
const CHARGING_MAX_MA: i32 = -50;
const DISCHARGE_MIN_MA: i32 = 10;

const BATTERY_EMPTY_MV: u32 = 6600;
const BATTERY_FULL_MV: u32 = 8200;

// Without a battery the charger output alternates between ~8.4 V and ~4.3 V
// every few seconds, so a dip below this marks the battery out for a while.
const ABSENT_MAX_MV: u32 = 6000;
const ABSENT_HOLD: Duration = Duration::from_secs(15);
const BATTERY_POLL: Duration = Duration::from_millis(500);
// A warm restart leaves the charger output up; this lets it fall and the INA226
// finish two conversions.
const BOOT_SETTLE: Duration = Duration::from_millis(100);

struct Latest {
    reading: Result<(u32, i32)>,
    absent_at: Option<Instant>,
}

struct BatterySource {
    ina: Ina226,
    latest: Arc<Mutex<Latest>>,
}

impl BatterySource {
    fn sample(&self) {
        let reading = self.ina.bus_voltage_mv().and_then(|mv| Ok((mv, self.ina.current_ma()?)));
        let mut latest = self.latest.lock();
        match reading {
            Ok((_, ma)) if !(CHARGING_MAX_MA < ma && ma < DISCHARGE_MIN_MA) => latest.absent_at = None,
            Ok((mv, _)) if mv < ABSENT_MAX_MV => latest.absent_at = Some(Instant::now()),
            Ok(_) => {}
            Err(err) => {
                if latest.reading.is_ok() {
                    warn!(TAG, "battery sensing: {err}");
                }
            }
        }
        latest.reading = reading;
    }
}

impl Source for BatterySource {
    fn tick(&mut self) -> Next {
        self.sample();
        Next::After(BATTERY_POLL)
    }
}

// Start while CHG_EN is still low, so the first sample sees the battery alone.
pub(crate) struct BatteryProbe {
    ina: Option<Ina226>,
    started: Instant,
}

impl BatteryProbe {
    pub(super) fn start(bus: &'static MasterBus) -> Self {
        let ina = Ina226::new(bus, INA226_ADDR, INA226_SHUNT_MOHM)
            .inspect_err(|err| warn!(TAG, "battery sensing unavailable: {err}"))
            .ok();
        Self { ina, started: Instant::now() }
    }
}

pub(crate) struct PowerDriver {
    latest: Option<Arc<Mutex<Latest>>>,
    io: &'static Mutex<Pi4io<'static>>,
}

impl PowerDriver {
    // Without the sensor the board can still switch off.
    pub(super) fn new(probe: BatteryProbe, io: &'static Mutex<Pi4io<'static>>) -> Self {
        let latest = probe.ina.and_then(|ina| {
            start_sensing(ina, probe.started)
                .inspect_err(|err| warn!(TAG, "battery polling unavailable: {err}"))
                .ok()
        });
        if let Err(err) = io.lock().set_output(CHG_EN_PIN, true) {
            warn!(TAG, "charger enable: {err}");
        }
        Self { latest, io }
    }
}

fn start_sensing(ina: Ina226, started: Instant) -> Result<Arc<Mutex<Latest>>> {
    thread::sleep(BOOT_SETTLE.saturating_sub(started.elapsed()));
    let latest = Arc::new(Mutex::new(Latest { reading: Ok((0, 0)), absent_at: None }));
    let source = BatterySource { ina, latest: latest.clone() };
    source.sample();
    dispatch::add(Box::new(source))?;
    Ok(latest)
}

impl Driver for PowerDriver {
    fn read(&self) -> Result<PowerState> {
        let latest = self.latest.as_ref().ok_or(Error::INVALID_STATE)?.lock();
        let (mv, ma) = latest.reading?;
        let source = if ma < DISCHARGE_MIN_MA { PowerSource::External } else { PowerSource::Battery };
        let battery = if latest.absent_at.is_some_and(|at| at.elapsed() < ABSENT_HOLD) {
            Battery::Absent
        } else {
            Battery::Present(BatteryState {
                level: power::linear_level(mv, BATTERY_EMPTY_MV, BATTERY_FULL_MV),
                voltage_mv: Some(mv),
                charging: Some(ma <= CHARGING_MAX_MA),
            })
        };
        Ok(PowerState { source, battery })
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
