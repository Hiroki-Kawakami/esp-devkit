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
use crate::devices::es8311::{self, Es8311};

const I2S_PORT: i32 = 0;
const I2S_PINS: Pins = Pins { mclk: 18, bclk: 17, ws: 15, dout: 21, din: -1 };

// Every open powers the codec up and configures it again, which cuts the head
// of a stream that follows closely.
const CLOSE_DELAY: Duration = Duration::from_secs(3);

pub(crate) struct AudioDriver {
    codec: Es8311,
}

impl AudioDriver {
    pub(crate) fn new(bus: &'static MasterBus, power: es8311::Switch, amp: es8311::Switch) -> Result<Self> {
        Ok(Self { codec: Es8311::new(bus, es8311::Config { port: I2S_PORT, pins: I2S_PINS, power, amp })? })
    }
}

impl audio::Driver for AudioDriver {
    type Buzzer = crate::Absent;

    fn open(&self, format: Format) -> Result<()> {
        self.codec.open(format)
    }

    fn close(&self) -> Result<()> {
        self.codec.close()
    }

    fn write(&self, data: &[u8]) -> Result<()> {
        self.codec.write(data)
    }

    fn close_delay(&self) -> Duration {
        CLOSE_DELAY
    }

    fn drain(&self) -> Result<()> {
        self.codec.drain()
    }

    fn queued(&self) -> Result<usize> {
        self.codec.queued()
    }

    fn hw_gain_min_db(&self) -> Option<f32> {
        Some(Es8311::gain_min_db())
    }

    fn set_hw_gain(&self, db: f32) -> Result<()> {
        self.codec.set_gain(db)
    }

    fn hw_mute(&self) -> HwMute {
        HwMute::Step
    }

    fn set_hw_mute(&self, mute: bool) -> Result<()> {
        self.codec.set_mute(mute)
    }

    fn has_speaker(&self) -> bool {
        true
    }

    fn set_speaker_enabled(&self, enabled: bool) -> Result<()> {
        self.codec.set_speaker_enabled(enabled)
    }

    // One speaker on the codec's left input, so both channels play mixed down.
    fn dsp_profile(&self, _headphone: bool, _sample_rate: u32) -> Option<DspProfile> {
        Some(DspProfile { biquads: [Biquad::default(); MAX_STAGES], stages: 0, eq_enabled: false, mono_mix: true })
    }
}
