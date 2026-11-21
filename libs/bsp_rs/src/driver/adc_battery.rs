/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! A battery read through a divider on an ADC pin, with the external supply
//! and the charger's state on GPIOs where the board has them.

use idf::adc::{Atten, Input};
use idf::{Result, gpio};

use crate::power::{self, Battery, BatteryState, PowerSource, PowerState};

const SAMPLES: u8 = 16;

#[derive(Clone, Copy)]
pub(crate) struct Level {
    pub gpio: i32,
    pub active_high: bool,
}

impl Level {
    fn init(self) -> Result<Self> {
        gpio::set_direction(self.gpio, gpio::Mode::Input)?;
        Ok(self)
    }

    fn active(self) -> bool {
        gpio::level(self.gpio) == self.active_high
    }
}

pub(crate) struct Config {
    pub gpio: i32,
    /// The battery's voltage over what the pin sees.
    pub divider: (u32, u32),
    pub empty_mv: u32,
    pub full_mv: u32,
    pub external: Option<Level>,
    pub charging: Option<Level>,
}

pub(crate) struct AdcBattery {
    input: Input,
    divider: (u32, u32),
    empty_mv: u32,
    full_mv: u32,
    external: Option<Level>,
    charging: Option<Level>,
}

impl AdcBattery {
    pub(crate) fn new(config: Config) -> Result<Self> {
        Ok(Self {
            input: Input::new(config.gpio, Atten::Db12)?,
            divider: config.divider,
            empty_mv: config.empty_mv,
            full_mv: config.full_mv,
            external: config.external.map(Level::init).transpose()?,
            charging: config.charging.map(Level::init).transpose()?,
        })
    }

    /// The battery is taken as present: the pin reads the charger's output
    /// without one.
    pub(crate) fn read(&self) -> Result<PowerState> {
        let (mul, div) = self.divider;
        let mv = self.input.read_mv(SAMPLES)? * mul / div;
        let external = self.external.map(Level::active);
        let source = match external {
            Some(true) => PowerSource::External,
            Some(false) => PowerSource::Battery,
            None => PowerSource::Unknown,
        };
        let charging = self.charging.map(|c| external != Some(false) && c.active());
        Ok(PowerState {
            source,
            battery: Battery::Present(BatteryState {
                level: power::linear_level(mv, self.empty_mv, self.full_mv),
                voltage_mv: Some(mv),
                charging,
            }),
        })
    }
}
