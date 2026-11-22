/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! A passive buzzer on an LEDC channel, sounding tones only: a square wave at
//! half duty, the pin held low while silent.

use idf::ledc::{self, Channel, ChannelConfig, TimerConfig};
use idf::{Error, Result};

use crate::audio::{BuzzerDriver, Driver, Format};

const RESOLUTION_BITS: u32 = 10;
const DUTY_ON: u32 = 1 << (RESOLUTION_BITS - 1);
const IDLE_HZ: u32 = 1000;

pub(crate) struct Config {
    pub gpio: i32,
    pub timer: u32,
    pub channel: u32,
}

pub(crate) struct PwmBuzzer {
    channel: Channel,
    timer: u32,
}

impl PwmBuzzer {
    pub(crate) fn new(config: &Config) -> Result<Self> {
        ledc::configure_timer(&TimerConfig { timer: config.timer, freq_hz: IDLE_HZ, resolution_bits: RESOLUTION_BITS })?;
        let channel = Channel::new(&ChannelConfig { channel: config.channel, timer: config.timer, gpio: config.gpio, duty: 0 })?;
        Ok(Self { channel, timer: config.timer })
    }
}

impl Driver for PwmBuzzer {
    type Buzzer = Self;

    fn open(&self, _format: Format) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }

    fn close(&self) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }

    fn write(&self, _data: &[u8]) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }

    fn buzzer(&self) -> Option<&Self> {
        Some(self)
    }

    fn streams(&self) -> bool {
        false
    }
}

impl BuzzerDriver for PwmBuzzer {
    fn sound(&self, hz: u32) -> Result<()> {
        if hz == 0 {
            return self.channel.set_duty(0);
        }
        ledc::set_freq(self.timer, hz)?;
        self.channel.set_duty(DUTY_ON)
    }
}
