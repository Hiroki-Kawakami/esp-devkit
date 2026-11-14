/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use idf::Result;

use crate::power::{self, Battery, PowerSource, PowerState};
use crate::{Absent, Board, Config};

pub(crate) type Display = Absent;
pub(crate) type AudioDriver = Absent;

struct UsbPower;

impl power::Driver for UsbPower {
    fn read(&self) -> Result<PowerState> {
        Ok(PowerState { source: PowerSource::External, battery: Battery::Absent })
    }
}

pub(crate) fn init(_config: &Config) -> Result<Board> {
    let power = power::start(UsbPower, None);
    Ok(Board { audio: None, display: None, imu: None, power, rtc: None, sd: None, touch: None })
}
