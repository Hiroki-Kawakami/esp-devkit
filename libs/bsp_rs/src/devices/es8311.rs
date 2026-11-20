/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::ffi::CStr;
use core::time::Duration;

use idf::i2c::{Device, MasterBus};
use idf::i2s::{Pins, StdFormat};
use idf::log::error;
use idf::sync::Mutex;
use idf::{Result, thread};

use crate::audio::Format;
use crate::driver::i2s_out::I2sOut;

const TAG: &CStr = c"es8311";

const ADDR: u16 = 0x18;
const SCL_HZ: u32 = 100_000;
const I2C_TIMEOUT: Duration = Duration::from_millis(100);
const POWER_SETTLE: Duration = Duration::from_millis(10);

const REG_RESET: u8 = 0x00;
const REG_CLK_MANAGER1: u8 = 0x01;
const REG_CLK_MANAGER2: u8 = 0x02;
const REG_SDP_IN: u8 = 0x09;
const REG_SYSTEM_0D: u8 = 0x0D;
const REG_SYSTEM_12: u8 = 0x12;
const REG_SYSTEM_13: u8 = 0x13;
const REG_DAC_31: u8 = 0x31;
const REG_DAC_VOLUME: u8 = 0x32;
const REG_DAC_37: u8 = 0x37;

const RESET_CSM_ON: u8 = 0x80;
// MCLK from its pin, with the DAC's clocks on.
const CLK_MANAGER1_DAC: u8 = 0x35;
// MCLK at 256 fs needs neither division nor multiplication.
const CLK_MANAGER2_256FS: u8 = 0x00;
const SYSTEM_0D_ANALOG_ON: u8 = 0x01;
const SYSTEM_12_DAC_ON: u8 = 0x00;
const SYSTEM_13_HP_DRIVE: u8 = 0x10;
const DAC_31_MUTE: u8 = 0x60;
const DAC_37_BYPASS_EQ: u8 = 0x08;

// 0.5 dB steps up from -95.5 dB; 0 dB sits at 0xBF.
const VOLUME_0DB: u8 = 0xBF;
const VOLUME_STEPS_PER_DB: f32 = 2.0;

/// `true` powers the part up; off, it forgets every register.
pub(crate) type Switch = Box<dyn Fn(bool) -> Result<()> + Send + Sync>;

pub(crate) struct Config {
    pub port: i32,
    /// With MCLK at 256 fs.
    pub pins: Pins,
    /// The codec's supply.
    pub power: Switch,
    /// The amplifier after the DAC.
    pub amp: Switch,
}

struct State {
    open: bool,
    speaker: bool,
    active: bool,
    bits: u32,
    volume: u8,
    mute: bool,
}

fn word_length(bits: u32) -> u8 {
    let code = match bits {
        16 => 0b011,
        32 => 0b100,
        _ => 0b000,
    };
    code << 2
}

/// Powered only while a stream is open with the speaker on, and configured
/// afresh each time it powers up.
pub(crate) struct Es8311 {
    device: Device<'static>,
    power: Switch,
    amp: Switch,
    out: I2sOut,
    state: Mutex<State>,
}

impl Es8311 {
    pub(crate) fn new(bus: &'static MasterBus, config: Config) -> Result<Self> {
        let device = bus.add_device(ADDR, SCL_HZ).inspect_err(|err| error!(TAG, "add device: {err}"))?;
        let out = I2sOut::new(config.port, config.pins)?;
        let state = State { open: false, speaker: false, active: false, bits: 16, volume: VOLUME_0DB, mute: true };
        Ok(Self { device, power: config.power, amp: config.amp, out, state: Mutex::new(state) })
    }

    fn write_reg(&self, reg: u8, value: u8) -> Result<()> {
        self.device.transmit(&[reg, value], I2C_TIMEOUT)
    }

    fn dac_31(state: &State) -> u8 {
        if state.mute { DAC_31_MUTE } else { 0 }
    }

    fn activate(&self, state: &mut State) -> Result<()> {
        (self.power)(true)?;
        state.active = true;
        thread::sleep(POWER_SETTLE);
        let configured = [
            (REG_RESET, RESET_CSM_ON),
            (REG_CLK_MANAGER1, CLK_MANAGER1_DAC),
            (REG_CLK_MANAGER2, CLK_MANAGER2_256FS),
            (REG_SDP_IN, word_length(state.bits)),
            (REG_DAC_31, Self::dac_31(state)),
            (REG_DAC_VOLUME, state.volume),
            (REG_DAC_37, DAC_37_BYPASS_EQ),
            (REG_SYSTEM_0D, SYSTEM_0D_ANALOG_ON),
            (REG_SYSTEM_12, SYSTEM_12_DAC_ON),
            (REG_SYSTEM_13, SYSTEM_13_HP_DRIVE),
        ]
        .into_iter()
        .try_for_each(|(reg, value)| self.write_reg(reg, value))
        .and_then(|()| (self.amp)(true));
        if let Err(err) = configured {
            error!(TAG, "configure: {err}");
            let _ = self.deactivate(state);
            return Err(err);
        }
        Ok(())
    }

    fn deactivate(&self, state: &mut State) -> Result<()> {
        let amp = (self.amp)(false);
        state.active = false;
        amp.and((self.power)(false))
    }

    fn configure(&self, format: &StdFormat) -> Result<()> {
        let mut state = self.state.lock();
        state.bits = format.bits;
        if state.active { self.write_reg(REG_SDP_IN, word_length(state.bits)) } else { Ok(()) }
    }

    /// Starts the stream, or changes the format of a running one.
    pub(crate) fn open(&self, format: Format) -> Result<()> {
        self.out.open(format, |std| self.configure(std))?;
        let mut state = self.state.lock();
        state.open = true;
        if state.speaker && !state.active { self.activate(&mut state) } else { Ok(()) }
    }

    // The codec goes down before the clocks do.
    pub(crate) fn close(&self) -> Result<()> {
        self.out.close(|| {
            let mut state = self.state.lock();
            state.open = false;
            if state.active { self.deactivate(&mut state) } else { Ok(()) }
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
        match (state.open && enabled, state.active) {
            (true, false) => self.activate(&mut state),
            (false, true) => self.deactivate(&mut state),
            _ => Ok(()),
        }
    }

    pub(crate) fn gain_min_db() -> f32 {
        -f32::from(VOLUME_0DB) / VOLUME_STEPS_PER_DB
    }

    pub(crate) fn set_gain(&self, db: f32) -> Result<()> {
        let mut state = self.state.lock();
        let below = (-db * VOLUME_STEPS_PER_DB + 0.5).clamp(0.0, f32::from(VOLUME_0DB)) as u8;
        state.volume = VOLUME_0DB - below;
        if state.active { self.write_reg(REG_DAC_VOLUME, state.volume) } else { Ok(()) }
    }

    pub(crate) fn set_mute(&self, mute: bool) -> Result<()> {
        let mut state = self.state.lock();
        state.mute = mute;
        if state.active { self.write_reg(REG_DAC_31, Self::dac_31(&state)) } else { Ok(()) }
    }
}
