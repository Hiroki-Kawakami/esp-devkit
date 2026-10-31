/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::time::Duration;

use devkit_audio::eq::Biquad;
use idf::Result;
use idf::i2c::MasterBus;
use idf::i2s::Pins;
use idf::sync::Mutex;

use crate::audio::{self, DspProfile, Format, HwMute, MAX_STAGES};
use crate::devices::es8388::{self, Es8388};
use crate::devices::pi4io::Pi4io;

const I2S_PORT: i32 = 0;
const I2S_PINS: Pins = Pins { mclk: 30, bclk: 27, ws: 29, dout: 26, din: 28 };

const SPK_EN_PIN: u8 = 1;
const HP_DET_PIN: u8 = 7;
// Spans a track change, so the codec's power click stays out of playback.
const CLOSE_DELAY: Duration = Duration::from_secs(5);

pub(crate) struct AudioDriver {
    codec: Es8388,
    io: &'static Mutex<Pi4io<'static>>,
}

impl AudioDriver {
    pub(crate) fn new(bus: &'static MasterBus, io: &'static Mutex<Pi4io<'static>>) -> Result<Self> {
        let codec = Es8388::new(bus, &es8388::Config {
            port: I2S_PORT,
            pins: I2S_PINS,
            outputs: es8388::OUT_LOUT1 | es8388::OUT_ROUT1,
        })?;
        Ok(Self { codec, io })
    }
}

impl audio::Driver for AudioDriver {
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
        Some(Es8388::gain_min_db())
    }

    fn set_hw_gain(&self, db: f32) -> Result<()> {
        self.codec.set_gain(db)
    }

    fn hw_mute(&self) -> HwMute {
        self.codec.mute_ramp()
    }

    fn set_hw_mute(&self, mute: bool) -> Result<()> {
        self.codec.set_mute(mute)
    }

    fn has_speaker(&self) -> bool {
        true
    }

    fn set_speaker_enabled(&self, enabled: bool) -> Result<()> {
        self.io.lock().set_output(SPK_EN_PIN, enabled)
    }

    fn has_headphone(&self) -> bool {
        true
    }

    // HP_DET is pulled up externally; the jack's NC detect switch shorts the line
    // to GND when no plug is inserted, so high means headphones in.
    fn headphone_inserted(&self) -> Result<bool> {
        self.io.lock().get_input(HP_DET_PIN)
    }

    // The speaker is LOUT1 only, so it gets a downmix shaped for the small
    // driver; the headphone is LOUT1/ROUT1 stereo.
    fn dsp_profile(&self, headphone: bool, sample_rate: u32) -> Option<DspProfile> {
        let fs = sample_rate;
        let mut biquads = [Biquad::default(); MAX_STAGES];
        let (stages, mono_mix) = if headphone {
            biquads[0] = Biquad::highpass(fs, 50.0, 0.707);
            biquads[1] = Biquad::low_shelf(fs, 150.0, 0.707, 10.0);
            biquads[2] = Biquad::peaking(fs, 1000.0, 0.80, -4.0);
            biquads[3] = Biquad::peaking(fs, 2500.0, 1.00, -3.0);
            (4, false)
        } else {
            biquads[0] = Biquad::highpass(fs, 80.0, 0.707);
            biquads[1] = Biquad::low_shelf(fs, 300.0, 0.707, 7.0);
            biquads[2] = Biquad::peaking(fs, 150.0, 1.20, 3.0);
            (3, true)
        };
        Some(DspProfile { biquads, stages, eq_enabled: true, mono_mix })
    }
}
