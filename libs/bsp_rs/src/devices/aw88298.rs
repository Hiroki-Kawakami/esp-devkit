/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::ffi::CStr;
use core::time::Duration;

use idf::i2c::{Device, MasterBus};
use idf::i2s::Pins;
use idf::log::error;
use idf::sync::Mutex;
use idf::Result;

use crate::audio::Format;
use crate::driver::i2s_out::I2sOut;

const TAG: &CStr = c"aw88298";

const ADDR: u16 = 0x36;
const SCL_HZ: u32 = 400_000;
const I2C_TIMEOUT: Duration = Duration::from_millis(100);

const REG_SYSCTRL: u8 = 0x04;
const REG_SYSCTRL2: u8 = 0x05;
const REG_I2SCTRL: u8 = 0x06;
const REG_HAGCCFG4: u8 = 0x0C;
const REG_BSTCTRL2: u8 = 0x61;

const SYSCTRL_RUN: u16 = 0x4040;
const SYSCTRL_STOP: u16 = 0x4000;
const SYSCTRL2_BASE: u16 = 0x0008;
// Stopping the output stage with AMPPD instead pops.
const SYSCTRL2_HMUTE: u16 = 1 << 4;
const BSTCTRL2_NO_BOOST: u16 = 0x0673;
const I2SCTRL_BASE: u16 = 0x14C0;
const HAGCCFG4_HOLDTH: u16 = 0x0064;

// The volume byte attenuates in 6 dB steps in its high nibble and 0.5 dB steps
// in its low one.
const GAIN_STEPS_PER_DB: f32 = 2.0;
const GAIN_FINE_STEPS: u8 = 12;
const GAIN_MIN_STEPS: u8 = 15 * GAIN_FINE_STEPS + GAIN_FINE_STEPS - 1;

/// Switches the chip's supply; it forgets every register while off.
pub(crate) type Power = Box<dyn Fn(bool) -> Result<()> + Send + Sync>;

pub(crate) struct Config {
    pub port: i32,
    pub pins: Pins,
    pub power: Power,
}

struct State {
    open: bool,
    speaker: bool,
    powered: bool,
    rate: u32,
    gain_steps: u8,
    mute: bool,
}

fn i2sctrl(sample_rate: u32) -> u16 {
    const RATES: [u32; 10] = [4, 5, 6, 8, 10, 11, 15, 20, 22, 44];
    let rate = (sample_rate + 1102) / 2205;
    let index = RATES.iter().position(|&r| rate <= r).unwrap_or(RATES.len());
    index as u16 | I2SCTRL_BASE
}

/// Powered only while a stream is open with the speaker on, and configured
/// afresh each time it powers up.
pub(crate) struct Aw88298 {
    device: Device<'static>,
    power: Power,
    out: I2sOut,
    state: Mutex<State>,
}

impl Aw88298 {
    pub(crate) fn new(bus: &'static MasterBus, config: Config) -> Result<Self> {
        let device = bus.add_device(ADDR, SCL_HZ).inspect_err(|err| error!(TAG, "add device: {err}"))?;
        let out = I2sOut::new(config.port, config.pins)?;
        let state = State { open: false, speaker: false, powered: false, rate: out.rate(), gain_steps: 0, mute: true };
        Ok(Self { device, power: config.power, out, state: Mutex::new(state) })
    }

    fn write_reg(&self, reg: u8, value: u16) -> Result<()> {
        let [hi, lo] = value.to_be_bytes();
        self.device.transmit(&[reg, hi, lo], I2C_TIMEOUT)
    }

    fn sysctrl2(state: &State) -> u16 {
        if state.mute { SYSCTRL2_BASE | SYSCTRL2_HMUTE } else { SYSCTRL2_BASE }
    }

    fn volume(state: &State) -> u16 {
        let byte = (state.gain_steps / GAIN_FINE_STEPS) << 4 | state.gain_steps % GAIN_FINE_STEPS;
        u16::from(byte) << 8 | HAGCCFG4_HOLDTH
    }

    fn power_up(&self, state: &mut State) -> Result<()> {
        (self.power)(true)?;
        state.powered = true;
        let configured = [
            (REG_BSTCTRL2, BSTCTRL2_NO_BOOST),
            (REG_SYSCTRL2, Self::sysctrl2(state)),
            (REG_I2SCTRL, i2sctrl(state.rate)),
            (REG_HAGCCFG4, Self::volume(state)),
            (REG_SYSCTRL, SYSCTRL_RUN),
        ]
        .into_iter()
        .try_for_each(|(reg, value)| self.write_reg(reg, value));
        if let Err(err) = configured {
            error!(TAG, "power up: {err}");
            let _ = self.power_down(state);
            return Err(err);
        }
        Ok(())
    }

    fn power_down(&self, state: &mut State) -> Result<()> {
        let stop = self.write_reg(REG_SYSCTRL, SYSCTRL_STOP);
        state.powered = false;
        stop.and((self.power)(false))
    }

    /// Starts the stream, or changes the format of a running one.
    pub(crate) fn open(&self, format: Format) -> Result<()> {
        self.out.open(format, |_| Ok(()))?;
        let mut state = self.state.lock();
        state.open = true;
        state.rate = format.sample_rate;
        if state.powered {
            self.write_reg(REG_I2SCTRL, i2sctrl(state.rate))
        } else if state.speaker {
            self.power_up(&mut state)
        } else {
            Ok(())
        }
    }

    // The chip goes down before the clocks do.
    pub(crate) fn close(&self) -> Result<()> {
        self.out.close(|| {
            let mut state = self.state.lock();
            state.open = false;
            if state.powered { self.power_down(&mut state) } else { Ok(()) }
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
        match (state.open && enabled, state.powered) {
            (true, false) => self.power_up(&mut state),
            (false, true) => self.power_down(&mut state),
            _ => Ok(()),
        }
    }

    pub(crate) fn gain_min_db() -> f32 {
        -f32::from(GAIN_MIN_STEPS) / GAIN_STEPS_PER_DB
    }

    pub(crate) fn set_gain(&self, db: f32) -> Result<()> {
        let mut state = self.state.lock();
        state.gain_steps = (-db * GAIN_STEPS_PER_DB + 0.5).clamp(0.0, f32::from(GAIN_MIN_STEPS)) as u8;
        if state.powered { self.write_reg(REG_HAGCCFG4, Self::volume(&state)) } else { Ok(()) }
    }

    pub(crate) fn set_mute(&self, mute: bool) -> Result<()> {
        let mut state = self.state.lock();
        state.mute = mute;
        if state.powered { self.write_reg(REG_SYSCTRL2, Self::sysctrl2(&state)) } else { Ok(()) }
    }
}
