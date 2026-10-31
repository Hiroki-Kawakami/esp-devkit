/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::ffi::{CStr, c_void};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering, fence};
use core::time::Duration;

use idf::i2c::{Device, MasterBus};
use idf::i2s::{self, Callbacks, Pins, StdFormat, TxChannel};
use idf::log::{error, warn};
use idf::sync::Mutex;
use idf::{Error, Result, sys, thread};

use crate::audio::{Format, HwMute, SampleFormat};

const TAG: &CStr = c"es8388";

const ADDR: u16 = 0x10;
const SCL_HZ: u32 = 100_000;
const I2C_TIMEOUT: Duration = Duration::from_millis(100);
const DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

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
const SILENCE_CHUNK: usize = 256;

const INITIAL_FORMAT: StdFormat = StdFormat { sample_rate: 48_000, bits: 16, channels: 2 };

/// DACPOWER output-enable bits.
pub(crate) const OUT_LOUT1: u8 = 0x20;
pub(crate) const OUT_ROUT1: u8 = 0x10;

pub(crate) struct Config {
    pub port: i32,
    pub pins: Pins,
    /// `OUT_*` bits.
    pub outputs: u8,
}

// Bytes into the DMA's output since the last enable: written data plays after
// `ahead`, and `sent` have played by `sent_us`. The ISR is the only writer of
// the sequenced fields, and a reader cannot preempt it, so reads always finish.
struct Counters {
    seq: AtomicU32,
    ahead: AtomicUsize,
    sent: AtomicUsize,
    buffer: AtomicUsize,
    sent_us: AtomicU32,
    written: AtomicUsize,
}

impl Counters {
    const fn new() -> Self {
        Self {
            seq: AtomicU32::new(0),
            ahead: AtomicUsize::new(0),
            sent: AtomicUsize::new(0),
            buffer: AtomicUsize::new(0),
            sent_us: AtomicU32::new(0),
            written: AtomicUsize::new(0),
        }
    }

    fn update(&self, f: impl FnOnce(&Self)) {
        let seq = self.seq.load(Ordering::Relaxed);
        self.seq.store(seq.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        f(self);
        self.seq.store(seq.wrapping_add(2), Ordering::Release);
    }

    /// With the channel disabled, so no ISR runs.
    fn reset(&self, ahead: usize) {
        self.update(|c| {
            c.ahead.store(ahead, Ordering::Relaxed);
            c.sent.store(0, Ordering::Relaxed);
            c.buffer.store(0, Ordering::Relaxed);
            c.sent_us.store(now_us(), Ordering::Relaxed);
        });
        self.written.store(0, Ordering::Relaxed);
    }

    fn snapshot(&self) -> Snapshot {
        loop {
            let seq = self.seq.load(Ordering::Acquire);
            let snapshot = Snapshot {
                ahead: self.ahead.load(Ordering::Relaxed),
                sent: self.sent.load(Ordering::Relaxed),
                buffer: self.buffer.load(Ordering::Relaxed),
                sent_us: self.sent_us.load(Ordering::Relaxed),
            };
            fence(Ordering::Acquire);
            if seq % 2 == 0 && self.seq.load(Ordering::Relaxed) == seq {
                return snapshot;
            }
        }
    }
}

struct Snapshot {
    ahead: usize,
    sent: usize,
    buffer: usize,
    sent_us: u32,
}

#[inline(always)]
fn now_us() -> u32 {
    unsafe { sys::esp_timer_get_time() as u32 }
}

#[unsafe(link_section = ".iram1.bsp_es8388_sent")]
unsafe extern "C" fn on_sent(
    _handle: sys::i2s_chan_handle_t,
    event: *mut sys::i2s_event_data_t,
    context: *mut c_void,
) -> bool {
    let counters = unsafe { &*context.cast::<Counters>() };
    let size = unsafe { (*event).size };
    counters.update(|c| {
        c.sent.store(c.sent.load(Ordering::Relaxed).wrapping_add(size), Ordering::Relaxed);
        c.buffer.store(size, Ordering::Relaxed);
        c.sent_us.store(now_us(), Ordering::Relaxed);
    });
    false
}

// A buffer the writer did not refill in time plays again, pushing every byte
// written after it one buffer later.
#[unsafe(link_section = ".iram1.bsp_es8388_ovf")]
unsafe extern "C" fn on_send_q_ovf(
    _handle: sys::i2s_chan_handle_t,
    event: *mut sys::i2s_event_data_t,
    context: *mut c_void,
) -> bool {
    let counters = unsafe { &*context.cast::<Counters>() };
    let size = unsafe { (*event).size };
    counters.update(|c| {
        c.ahead.store(c.ahead.load(Ordering::Relaxed).wrapping_add(size), Ordering::Relaxed);
    });
    false
}

fn std_format(format: Format) -> StdFormat {
    let bits = match format.sample {
        SampleFormat::S16 => 16,
        SampleFormat::S32 => 32,
    };
    StdFormat { sample_rate: format.sample_rate, bits, channels: format.channels }
}

fn word_length(bits: u32) -> u8 {
    match bits {
        24 => 0x00,
        32 => 0x04,
        _ => 0x03,
    }
}

struct Tx {
    channel: Option<TxChannel>,
    enabled: bool,
    ring_bytes: usize,
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
    port: i32,
    pins: Pins,
    outputs: u8,
    counters: Box<Counters>,
    // Held across blocking writes, so only the stream's owner takes it.
    tx: Mutex<Tx>,
    enabled: AtomicBool,
    rate: AtomicU32,
    frame_bytes: AtomicUsize,
}

impl Es8388 {
    pub(crate) fn new(bus: &'static MasterBus, config: &Config) -> Result<Self> {
        let device = bus.add_device(ADDR, SCL_HZ).inspect_err(|err| error!(TAG, "add device: {err}"))?;
        let counters = Box::new(Counters::new());
        let channel = TxChannel::new(config.port, INITIAL_FORMAT, config.pins)
            .inspect_err(|err| error!(TAG, "i2s channel: {err}"))?;
        let codec = Self {
            device,
            port: config.port,
            pins: config.pins,
            outputs: config.outputs,
            counters,
            tx: Mutex::try_new(Tx { channel: None, enabled: false, ring_bytes: 0 })?,
            enabled: AtomicBool::new(false),
            rate: AtomicU32::new(INITIAL_FORMAT.sample_rate),
            frame_bytes: AtomicUsize::new(0),
        };
        codec.register_callbacks(&channel).inspect_err(|err| error!(TAG, "i2s callbacks: {err}"))?;
        codec.tx.lock().channel = Some(channel);
        codec.reset().inspect_err(|err| error!(TAG, "codec reset: {err}"))?;
        Ok(codec)
    }

    fn register_callbacks(&self, channel: &TxChannel) -> Result<()> {
        let callbacks = Callbacks { on_sent: Some(on_sent), on_send_q_ovf: Some(on_send_q_ovf) };
        let context = (&raw const *self.counters).cast_mut().cast();
        unsafe { channel.set_callbacks(&callbacks, context) }
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

    // A failed slot reconfig leaves the channel disabled with its previous DMA
    // descriptors still held, so every retry hits the same shortage. Rebuilding
    // the channel releases them, the one recovery without a device reset.
    fn recreate_channel(&self, tx: &mut Tx, format: StdFormat) -> Result<()> {
        tx.channel = None;
        let channel = TxChannel::new(self.port, format, self.pins)?;
        self.register_callbacks(&channel)?;
        tx.channel = Some(channel);
        Ok(())
    }

    fn apply_format(&self, tx: &mut Tx, format: StdFormat) -> Result<()> {
        if tx.enabled {
            tx.channel.as_ref().ok_or(Error::INVALID_STATE)?.disable()?;
            tx.enabled = false;
            self.enabled.store(false, Ordering::Release);
        }
        let reconfigured = match &tx.channel {
            Some(channel) => channel.reconfigure(format),
            None => Err(Error::FAIL),
        };
        if let Err(err) = reconfigured {
            warn!(TAG, "i2s reconfigure: {err}, recreating the channel");
            self.recreate_channel(tx, format)
                .inspect_err(|err| error!(TAG, "i2s recreate: {err}"))?;
        }
        self.write_reg(REG_DACCONTROL1, word_length(format.bits) << 3)?;

        let channel = tx.channel.as_ref().ok_or(Error::INVALID_STATE)?;
        tx.ring_bytes = channel.ring_bytes().unwrap_or(0);
        // Enabling starts the DMA on the ring as it is; the writer gets each
        // buffer once it has played, so the first byte written plays a ring later.
        self.counters.reset(tx.ring_bytes);
        channel.enable()?;
        tx.enabled = true;
        self.rate.store(format.sample_rate, Ordering::Relaxed);
        self.frame_bytes.store(usize::from(format.channels) * format.bits as usize / 8, Ordering::Relaxed);
        self.enabled.store(true, Ordering::Release);
        Ok(())
    }

    /// Starts the stream, or changes the format of a running one. The output
    /// stays muted.
    pub(crate) fn open(&self, format: Format) -> Result<()> {
        self.write_reg(REG_DACPOWER, DACPOWER_OFF)?;
        let mut tx = self.tx.lock();
        self.apply_format(&mut tx, std_format(format))?;
        self.write_reg(REG_DACPOWER, self.outputs)
    }

    // The output drivers go down before the clocks do.
    pub(crate) fn close(&self) -> Result<()> {
        let mut tx = self.tx.lock();
        if !tx.enabled {
            return Ok(());
        }
        let mute = self.set_mute(true);
        let power = self.write_reg(REG_DACPOWER, DACPOWER_OFF);
        tx.enabled = false;
        self.enabled.store(false, Ordering::Release);
        let disable = tx.channel.as_ref().ok_or(Error::INVALID_STATE)?.disable();
        mute.and(power).and(disable)
    }

    pub(crate) fn write(&self, data: &[u8]) -> Result<()> {
        let tx = self.tx.lock();
        if !tx.enabled {
            return Err(Error::INVALID_STATE);
        }
        let channel = tx.channel.as_ref().ok_or(Error::INVALID_STATE)?;
        let (written, result) = channel.write(data, i2s::FOREVER);
        self.counters.written.fetch_add(written, Ordering::Relaxed);
        result
    }

    /// Frames written that have not played yet.
    pub(crate) fn queued(&self) -> Result<usize> {
        if !self.enabled.load(Ordering::Acquire) {
            return Err(Error::INVALID_STATE);
        }
        let frame_bytes = self.frame_bytes.load(Ordering::Relaxed);
        let rate = u64::from(self.rate.load(Ordering::Relaxed));
        let written = self.counters.written.load(Ordering::Relaxed);
        let snapshot = self.counters.snapshot();
        let since = u64::from(now_us().wrapping_sub(snapshot.sent_us));
        let mut playing = (since * rate / 1_000_000) as usize * frame_bytes;
        if snapshot.buffer != 0 {
            playing = playing.min(snapshot.buffer);
        }
        let end = snapshot.ahead.wrapping_add(written);
        let played = snapshot.sent.wrapping_add(playing);
        let left = end.wrapping_sub(played) as isize;
        Ok(left.max(0) as usize / frame_bytes)
    }

    // i2s_channel_disable() leaves the DMA buffers holding whatever was queued,
    // and they are only reallocated when the buffer size changes, so a stream
    // that stops mid-playback and reopens at the same width replays that audio.
    // A full ring of silence fixes both halves: write() only returns a buffer
    // once it was sent, so everything queued before it has left the codec, and
    // the ring is left holding zeros.
    pub(crate) fn drain(&self) -> Result<()> {
        let tx = self.tx.lock();
        if !tx.enabled {
            return Ok(());
        }
        let channel = tx.channel.as_ref().ok_or(Error::INVALID_STATE)?;
        let silence = [0u8; SILENCE_CHUNK];
        let mut remain = tx.ring_bytes;
        while remain > 0 {
            let (written, result) = channel.write(&silence[..remain.min(SILENCE_CHUNK)], DRAIN_TIMEOUT);
            self.counters.written.fetch_add(written, Ordering::Relaxed);
            result?;
            if written == 0 {
                break;
            }
            remain -= written;
        }
        Ok(())
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
        let rate = self.rate.load(Ordering::Relaxed).max(1);
        let ms = (FULL_RAMP_FRAMES * 1000).div_ceil(rate) + RAMP_MARGIN_MS;
        HwMute::Ramp(Duration::from_millis(u64::from(ms)))
    }

    pub(crate) fn set_mute(&self, mute: bool) -> Result<()> {
        let bit = if mute { DAC_MUTE_BIT } else { 0 };
        self.write_reg(REG_DACCONTROL3, DACCONTROL3_BASE | bit)
    }
}
