/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;
use core::time::Duration;

use idf::i2c::{Device, MasterBus};
use idf::i2s::Pins;
use idf::log::error;
use idf::{Result, thread};

use crate::audio::{Format, HwMute};
use crate::driver::i2s_out::I2sOut;

const TAG: &CStr = c"es8388";

const ADDR: u16 = 0x10;
const SCL_HZ: u32 = 100_000;
const I2C_TIMEOUT: Duration = Duration::from_millis(100);

const REG_CONTROL1: u8 = 0x00;
const REG_CONTROL2: u8 = 0x01;
const REG_CHIPPOWER: u8 = 0x02;
const REG_ADCPOWER: u8 = 0x03;
const REG_DACPOWER: u8 = 0x04;
const REG_MASTERMODE: u8 = 0x08;
const REG_DACCONTROL1: u8 = 0x17;
const REG_DACCONTROL2: u8 = 0x18;
const REG_DACCONTROL3: u8 = 0x19;
const REG_DACCONTROL4: u8 = 0x1a;
const REG_DACCONTROL5: u8 = 0x1b;
const REG_DACCONTROL16: u8 = 0x26;
const REG_DACCONTROL17: u8 = 0x27;
const REG_DACCONTROL20: u8 = 0x2a;
const REG_DACCONTROL21: u8 = 0x2b;
const REG_DACCONTROL23: u8 = 0x2d;
const REG_DACCONTROL24: u8 = 0x2e;
const REG_DACCONTROL25: u8 = 0x2f;
const REG_DACCONTROL26: u8 = 0x30;
const REG_DACCONTROL27: u8 = 0x31;

const DAC_MUTE_BIT: u8 = 0x04;
const DACCONTROL3_BASE: u8 = 0x32;
const DACPOWER_OFF: u8 = 0xC0;
const CHIPPOWER_IDLE: u8 = 0xF2;
const CHIPPOWER_ACTIVE: u8 = 0xAA;

const CONTROL1_CHARGE: u8 = 0x07;
const CONTROL1_RUN: u8 = 0x05;
const CONTROL2_RUN: u8 = 0x40;

// Enabling an output driver is only silent if the pin is already sitting at
// Vmid, so Vmid must have finished charging the board's decoupling capacitor
// first. Through the 50k divider that takes hundreds of ms; the 5k divider
// charges it in tens, so Vmid comes up on 5k and drops to 50k for running.
const VMID_CHARGE: Duration = Duration::from_millis(100);
const VMID_SETTLE: Duration = Duration::from_millis(30);

const DAC_VOLUME_MIN_REG: u8 = 192;
const DAC_VOLUME_STEPS_PER_DB: f32 = 2.0;
// DACSoftRamp moves 0.5 dB per 4 LRCK at the rate in DACCONTROL3_BASE, so a mute
// from 0 dB takes this many frames at whatever rate is playing.
const FULL_RAMP_FRAMES: u32 = DAC_VOLUME_MIN_REG as u32 * 4;
const RAMP_MARGIN_MS: u32 = 4;

/// DACPOWER output-enable bits.
pub(crate) const OUT_LOUT1: u8 = 0x20;
pub(crate) const OUT_ROUT1: u8 = 0x10;

pub(crate) struct Config {
    pub port: i32,
    pub pins: Pins,
    /// `OUT_*` bits.
    pub outputs: u8,
}

fn word_length(bits: u32) -> u8 {
    match bits {
        24 => 0x00,
        32 => 0x04,
        _ => 0x03,
    }
}

// Measured on this codec; reordering it is audible on headphones.
// - The output drivers may only be powered while the I2S clock runs. With no
//   MCLK the DAC puts out full-scale noise, and the mute does not cover it
//   because its soft ramp is clocked by LRCK.
// - Every driver power transition clicks at a level no volume attenuates, and
//   none of SeqEn, VROI, the soft ramp or the attenuator changes it. Each
//   open/close pair costs one click, so quiet playback means opening less.
pub(crate) struct Es8388 {
    device: Device<'static>,
    outputs: u8,
    out: I2sOut,
}

impl Es8388 {
    pub(crate) fn new(bus: &'static MasterBus, config: &Config) -> Result<Self> {
        let device = bus.add_device(ADDR, SCL_HZ).inspect_err(|err| error!(TAG, "add device: {err}"))?;
        let out = I2sOut::new(config.port, config.pins)?;
        let codec = Self { device, outputs: config.outputs, out };
        codec.reset().inspect_err(|err| error!(TAG, "codec reset: {err}"))?;
        Ok(codec)
    }

    fn write_reg(&self, reg: u8, value: u8) -> Result<()> {
        self.device.transmit(&[reg, value], I2C_TIMEOUT)
    }

    fn read_reg(&self, reg: u8) -> Result<u8> {
        let mut value = [0u8];
        self.device.transmit_receive(&[reg], &mut value, I2C_TIMEOUT)?;
        Ok(value[0])
    }

    // The reference and Vmid come up here, once, and stay up for the life of the
    // device; only the output drivers follow open/close. Enabling a driver is
    // silent when the pin is already at Vmid, which VROI holds through 1.5k, and
    // the board cuts VSYS on a host reset, where a driver that is off lets the
    // output decay through that same 1.5k instead of collapsing through the
    // driver.
    fn reset(&self) -> Result<()> {
        // A host reset may leave the codec powered and already configured; Vmid
        // is then up and its charge wait would be dead time.
        let reference_up = self.read_reg(REG_CONTROL2) == Ok(CONTROL2_RUN);
        let control1 = if reference_up { CONTROL1_RUN } else { CONTROL1_CHARGE };

        let sequence: [(u8, u8); 23] = [
            (REG_DACCONTROL3, DACCONTROL3_BASE | DAC_MUTE_BIT),
            (REG_CHIPPOWER, CHIPPOWER_IDLE),
            (REG_DACPOWER, DACPOWER_OFF),
            (REG_MASTERMODE, 0x00),
            (REG_DACCONTROL21, 0x80),
            (REG_CONTROL1, control1),
            (REG_CONTROL2, CONTROL2_RUN),
            // The internal DLL hurts low sample rates.
            (0x35, 0xA0),
            (0x37, 0xD0),
            (0x39, 0xD0),
            (REG_DACCONTROL1, 0x18),
            (REG_DACCONTROL2, 0x02),
            (REG_DACCONTROL16, 0x00),
            (REG_DACCONTROL17, 0x90),
            (REG_DACCONTROL20, 0x90),
            (REG_DACCONTROL23, 0x00),
            (REG_DACCONTROL4, 0x00),
            (REG_DACCONTROL5, 0x00),
            (REG_DACCONTROL24, 0x1E),
            (REG_DACCONTROL25, 0x1E),
            (REG_DACCONTROL26, 0x1E),
            (REG_DACCONTROL27, 0x1E),
            (REG_ADCPOWER, 0xFF),
        ];
        for (reg, value) in sequence {
            self.write_reg(reg, value)?;
        }

        if !reference_up {
            thread::sleep(VMID_CHARGE);
            self.write_reg(REG_CONTROL1, CONTROL1_RUN)?;
            thread::sleep(VMID_SETTLE);
        }
        self.write_reg(REG_CHIPPOWER, CHIPPOWER_ACTIVE)
    }

    /// Starts the stream, or changes the format of a running one. The output
    /// stays muted.
    pub(crate) fn open(&self, format: Format) -> Result<()> {
        self.write_reg(REG_DACPOWER, DACPOWER_OFF)?;
        self.out.open(format, |std| self.write_reg(REG_DACCONTROL1, word_length(std.bits) << 3))?;
        self.write_reg(REG_DACPOWER, self.outputs)
    }

    // The output drivers go down before the clocks do.
    pub(crate) fn close(&self) -> Result<()> {
        self.out.close(|| {
            let mute = self.set_mute(true);
            mute.and(self.write_reg(REG_DACPOWER, DACPOWER_OFF))
        })
    }

    pub(crate) fn write(&self, data: &[u8]) -> Result<()> {
        self.out.write(data)
    }

    /// Frames written that have not played yet.
    pub(crate) fn queued(&self) -> Result<usize> {
        self.out.queued()
    }

    pub(crate) fn drain(&self) -> Result<()> {
        self.out.drain()
    }

    pub(crate) const fn gain_min_db() -> f32 {
        -(DAC_VOLUME_MIN_REG as f32) / DAC_VOLUME_STEPS_PER_DB
    }

    pub(crate) fn set_gain(&self, db: f32) -> Result<()> {
        let reg = (-db * DAC_VOLUME_STEPS_PER_DB + 0.5).clamp(0.0, f32::from(DAC_VOLUME_MIN_REG)) as u8;
        self.write_reg(REG_DACCONTROL4, reg)?;
        self.write_reg(REG_DACCONTROL5, reg)
    }

    pub(crate) fn mute_ramp(&self) -> HwMute {
        let rate = self.out.rate().max(1);
        let ms = (FULL_RAMP_FRAMES * 1000).div_ceil(rate) + RAMP_MARGIN_MS;
        HwMute::Ramp(Duration::from_millis(u64::from(ms)))
    }

    pub(crate) fn set_mute(&self, mute: bool) -> Result<()> {
        let bit = if mute { DAC_MUTE_BIT } else { 0 };
        self.write_reg(REG_DACCONTROL3, DACCONTROL3_BASE | bit)
    }
}
