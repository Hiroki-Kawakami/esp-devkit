/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;

use idf::Result;
use idf::i2s::Pins;
use idf::sync::Mutex;

use crate::audio::Format;
use crate::driver::i2s_out::I2sOut;

/// Drives the amp's shutdown pin: `true` lets it play.
pub(crate) type Enable = Box<dyn Fn(bool) -> Result<()> + Send + Sync>;

pub(crate) struct Config {
    pub port: i32,
    pub pins: Pins,
    pub enable: Enable,
}

struct State {
    open: bool,
    speaker: bool,
    active: bool,
}

/// An I2S amp without registers, on only while a stream is open with the
/// speaker on.
pub(crate) struct Ns4168 {
    enable: Enable,
    out: I2sOut,
    state: Mutex<State>,
}

impl Ns4168 {
    pub(crate) fn new(config: Config) -> Result<Self> {
        let out = I2sOut::new(config.port, config.pins)?;
        (config.enable)(false)?;
        Ok(Self { enable: config.enable, out, state: Mutex::new(State { open: false, speaker: false, active: false }) })
    }

    fn set_active(&self, state: &mut State, active: bool) -> Result<()> {
        if state.active != active {
            (self.enable)(active)?;
            state.active = active;
        }
        Ok(())
    }

    /// Starts the stream, or changes the format of a running one.
    pub(crate) fn open(&self, format: Format) -> Result<()> {
        self.out.open(format, |_| Ok(()))?;
        let mut state = self.state.lock();
        state.open = true;
        let on = state.speaker;
        self.set_active(&mut state, on)
    }

    // The amp goes down before the clocks do.
    pub(crate) fn close(&self) -> Result<()> {
        self.out.close(|| {
            let mut state = self.state.lock();
            state.open = false;
            self.set_active(&mut state, false)
        })
    }

    pub(crate) fn write(&self, data: &[u8]) -> Result<()> {
        self.out.write(data)
    }

    pub(crate) fn queued(&self) -> Result<usize> {
        self.out.queued()
    }

    pub(crate) fn drain(&self) -> Result<()> {
        self.out.drain()
    }

    pub(crate) fn set_speaker_enabled(&self, enabled: bool) -> Result<()> {
        let mut state = self.state.lock();
        state.speaker = enabled;
        let on = state.open && enabled;
        self.set_active(&mut state, on)
    }
}
