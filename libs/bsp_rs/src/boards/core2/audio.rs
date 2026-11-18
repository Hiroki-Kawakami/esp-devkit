/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit_audio::eq::Biquad;
use idf::Result;
use idf::i2s::Pins;

use crate::audio::{self, DspProfile, Format, MAX_STAGES};
use crate::devices::ns4168::{self, Ns4168};

const I2S_PORT: i32 = 0;
const I2S_PINS: Pins = Pins { mclk: -1, bclk: 12, ws: 0, dout: 2, din: -1 };

pub(crate) struct AudioDriver {
    amp: Ns4168,
}

impl AudioDriver {
    pub(crate) fn new(enable: ns4168::Enable) -> Result<Self> {
        Ok(Self { amp: Ns4168::new(ns4168::Config { port: I2S_PORT, pins: I2S_PINS, enable })? })
    }
}

impl audio::Driver for AudioDriver {
    fn open(&self, format: Format) -> Result<()> {
        self.amp.open(format)
    }

    fn close(&self) -> Result<()> {
        self.amp.close()
    }

    fn write(&self, data: &[u8]) -> Result<()> {
        self.amp.write(data)
    }

    fn drain(&self) -> Result<()> {
        self.amp.drain()
    }

    fn queued(&self) -> Result<usize> {
        self.amp.queued()
    }

    fn has_speaker(&self) -> bool {
        true
    }

    fn set_speaker_enabled(&self, enabled: bool) -> Result<()> {
        self.amp.set_speaker_enabled(enabled)
    }

    // One speaker, so both channels play mixed down.
    fn dsp_profile(&self, _headphone: bool, _sample_rate: u32) -> Option<DspProfile> {
        Some(DspProfile { biquads: [Biquad::default(); MAX_STAGES], stages: 0, eq_enabled: false, mono_mix: true })
    }
}
