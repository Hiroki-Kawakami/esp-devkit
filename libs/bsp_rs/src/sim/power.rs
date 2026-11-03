/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use idf::{Error, Result};

use core::ffi::CStr;

use super::protocol::{ENV_POWER, parse_power};
use super::{Host, ffi};
use crate::power::{self, BatteryState, Charge, Driver, PowerSource, PowerState};

const EMPTY_MV: u32 = 6600;
const FULL_MV: u32 = 8200;

const LEVEL_MASK: u32 = 0xFF;
const EXTERNAL: u32 = 1 << 8;
const BATTERY: u32 = 1 << 9;

fn word(level: u8, external: bool, battery: bool) -> u32 {
    u32::from(level.min(100)) | if external { EXTERNAL } else { 0 } | if battery { BATTERY } else { 0 }
}

// One word, so a reader never sees a torn state. Without a host there is no
// battery and external power, as on a bench supply.
pub(crate) struct SimPower {
    state: Arc<AtomicU32>,
    charge: AtomicBool,
}

impl SimPower {
    pub(crate) fn new(host: Option<&'static Host>) -> Self {
        let initial = host.and_then(|_| {
            let value = unsafe { ffi::getenv(ENV_POWER.as_ptr()) };
            if value.is_null() {
                return None;
            }
            parse_power(unsafe { CStr::from_ptr(value) }.to_str().ok()?)
        });
        let state = Arc::new(AtomicU32::new(initial.map_or(EXTERNAL, |(l, e, b)| word(l, e, b))));
        if let Some(host) = host {
            let listener = state.clone();
            host.set_power_listener(Box::new(move |level, external, battery| {
                listener.store(word(level, external, battery), Ordering::Relaxed);
            }));
        }
        Self { state, charge: AtomicBool::new(true) }
    }
}

impl Driver for SimPower {
    fn read(&self) -> Result<PowerState> {
        let word = self.state.load(Ordering::Relaxed);
        let external = word & EXTERNAL != 0;
        let battery = (word & BATTERY != 0).then(|| {
            let level = (word & LEVEL_MASK) as u8;
            let mv = EMPTY_MV + u32::from(level) * (FULL_MV - EMPTY_MV) / 100;
            BatteryState {
                level: power::linear_level(mv, EMPTY_MV, FULL_MV),
                voltage_mv: Some(mv),
                charging: Some(external && level < 100 && self.charge.load(Ordering::Relaxed)),
            }
        });
        let source = if external { PowerSource::External } else { PowerSource::Battery };
        Ok(PowerState { source: Some(source), battery })
    }

    fn set_charge(&self, charge: Charge) -> Result<()> {
        self.charge.store(charge != Charge::Off, Ordering::Relaxed);
        Ok(())
    }

    // The host shows the firmware as stopped, and Power on starts it again.
    fn off(&self) -> Error {
        unsafe { ffi::exit(0) }
    }
}
