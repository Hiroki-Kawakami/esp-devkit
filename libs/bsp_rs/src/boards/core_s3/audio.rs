/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::time::Duration;

use devkit_audio::eq::Biquad;
use idf::Result;
use idf::i2c::MasterBus;
use idf::i2s::Pins;

use crate::audio::{self, DspProfile, Format, HwMute, MAX_STAGES};
use crate::devices::aw88298::{self, Aw88298};

const I2S_PORT: i32 = 1;
const I2S_PINS: Pins = Pins { mclk: -1, bclk: 34, ws: 33, dout: 13, din: -1 };

// Every open powers the amp up and configures it again, which cuts the head
// of a stream that follows closely.
const CLOSE_DELAY: Duration = Duration::from_secs(3);

pub(crate) struct AudioDriver {
    amp: Aw88298,
}

impl AudioDriver {
    pub(crate) fn new(bus: &'static MasterBus, power: aw88298::Power) -> Result<Self> {
        Ok(Self { amp: Aw88298::new(bus, aw88298::Config { port: I2S_PORT, pins: I2S_PINS, power })? })
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

    fn close_delay(&self) -> Duration {
        CLOSE_DELAY
    }

    fn drain(&self) -> Result<()> {
        self.amp.drain()
    }

    fn queued(&self) -> Result<usize> {
        self.amp.queued()
    }

    fn hw_gain_min_db(&self) -> Option<f32> {
        Some(Aw88298::gain_min_db())
    }

    fn set_hw_gain(&self, db: f32) -> Result<()> {
        self.amp.set_gain(db)
    }

    fn hw_mute(&self) -> HwMute {
        HwMute::Step
    }

    fn set_hw_mute(&self, mute: bool) -> Result<()> {
        self.amp.set_mute(mute)
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
