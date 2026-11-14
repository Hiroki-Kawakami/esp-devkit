/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;

use idf::sync::OnceLock;
use idf::{Error, Result, system};

use crate::audio::Audio;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PowerSource {
    Unknown,
    Battery,
    External,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BatteryState {
    /// 0..=100. A linear map of the terminal voltage on most boards, so a
    /// coarse gauge rather than a state of charge.
    pub level: u8,
    pub voltage_mv: Option<u32>,
    /// `None` when the board cannot tell.
    pub charging: Option<bool>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Battery {
    Unknown,
    Absent,
    Present(BatteryState),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PowerState {
    pub source: PowerSource,
    pub battery: Battery,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Charge {
    Off,
    Normal,
    Fast,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Switch {
    /// USB host port power.
    Usb5V,
}

pub(crate) trait Driver: Send + Sync + 'static {
    fn read(&self) -> Result<PowerState> {
        Ok(PowerState { source: PowerSource::Unknown, battery: Battery::Unknown })
    }

    fn set_charge(&self, _charge: Charge) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }

    fn set_switch(&self, _switch: Switch, _on: bool) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }

    /// Returns only when the board stays on.
    fn off(&self) -> Error {
        Error::NOT_SUPPORTED
    }

    fn restart(&self) -> ! {
        system::restart()
    }
}

/// `empty_mv..=full_mv` mapped linearly onto 0..=100.
pub(crate) fn linear_level(mv: u32, empty_mv: u32, full_mv: u32) -> u8 {
    if mv <= empty_mv {
        0
    } else if mv >= full_mv {
        100
    } else {
        ((mv - empty_mv) * 100 / (full_mv - empty_mv)) as u8
    }
}

/// Shared by every task.
pub struct Power {
    driver: Box<dyn Driver>,
    audio: Option<&'static Audio>,
}

impl Power {
    pub fn read(&self) -> Result<PowerState> {
        self.driver.read()
    }

    /// `NOT_SUPPORTED` for a mode the charger lacks.
    pub fn set_charge(&self, charge: Charge) -> Result<()> {
        self.driver.set_charge(charge)
    }

    pub fn set_switch(&self, switch: Switch, on: bool) -> Result<()> {
        self.driver.set_switch(switch, on)
    }

    /// Cuts the power. Returns only when it stays on, as external power holds
    /// some boards up, and then everything keeps working.
    pub fn off(&self) -> Error {
        let quiet = self.audio.map(Audio::quiesce);
        let err = self.driver.off();
        if let (Some(audio), Some(quiet)) = (self.audio, quiet) {
            audio.resume(quiet);
        }
        err
    }

    /// A soft reboot.
    pub fn restart(&self) -> ! {
        if let Some(audio) = self.audio {
            audio.quiesce();
        }
        self.driver.restart()
    }
}

static POWER: OnceLock<Power> = OnceLock::new();

pub(crate) fn start(driver: impl Driver, audio: Option<&'static Audio>) -> &'static Power {
    POWER.get_or_init(|| Power { driver: Box::new(driver), audio })
}

#[cfg(test)]
mod tests {
    use super::linear_level;

    #[test]
    fn level_is_linear_between_the_endpoints() {
        assert_eq!(linear_level(6000, 6600, 8200), 0);
        assert_eq!(linear_level(6600, 6600, 8200), 0);
        assert_eq!(linear_level(7400, 6600, 8200), 50);
        assert_eq!(linear_level(8200, 6600, 8200), 100);
        assert_eq!(linear_level(9000, 6600, 8200), 100);
    }
}
