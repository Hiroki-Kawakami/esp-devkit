/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::ffi::{CStr, c_void};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering, fence};
use core::time::Duration;

use idf::i2s::{self, Callbacks, Pins, StdFormat, TxChannel};
use idf::log::{error, warn};
use idf::sync::Mutex;
use idf::{Error, Result, sys};

use crate::audio::{Format, SampleFormat};

const TAG: &CStr = c"i2s_out";

const DRAIN_TIMEOUT: Duration = Duration::from_millis(500);
const SILENCE_CHUNK: usize = 256;
const INITIAL_FORMAT: StdFormat = StdFormat { sample_rate: 48_000, bits: 16, channels: 2 };

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

#[unsafe(link_section = ".iram1.bsp_i2s_out_sent")]
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
#[unsafe(link_section = ".iram1.bsp_i2s_out_ovf")]
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

struct Tx {
    channel: Option<TxChannel>,
    enabled: bool,
    ring_bytes: usize,
}

/// A standard I2S transmit stream that knows how much of what was written has
/// played.
pub(crate) struct I2sOut {
    port: i32,
    pins: Pins,
    counters: Box<Counters>,
    // Held across blocking writes, so only the stream's owner takes it.
    tx: Mutex<Tx>,
    enabled: AtomicBool,
    rate: AtomicU32,
    frame_bytes: AtomicUsize,
}

impl I2sOut {
    pub(crate) fn new(port: i32, pins: Pins) -> Result<Self> {
        let channel = TxChannel::new(port, INITIAL_FORMAT, pins).inspect_err(|err| error!(TAG, "i2s channel: {err}"))?;
        let out = Self {
            port,
            pins,
            counters: Box::new(Counters::new()),
            tx: Mutex::new(Tx { channel: None, enabled: false, ring_bytes: 0 }),
            enabled: AtomicBool::new(false),
            rate: AtomicU32::new(INITIAL_FORMAT.sample_rate),
            frame_bytes: AtomicUsize::new(0),
        };
        out.register_callbacks(&channel).inspect_err(|err| error!(TAG, "i2s callbacks: {err}"))?;
        out.tx.lock().channel = Some(channel);
        Ok(out)
    }

    fn register_callbacks(&self, channel: &TxChannel) -> Result<()> {
        let callbacks = Callbacks { on_sent: Some(on_sent), on_send_q_ovf: Some(on_send_q_ovf) };
        let context = (&raw const *self.counters).cast_mut().cast();
        unsafe { channel.set_callbacks(&callbacks, context) }
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

    /// Starts the stream, or changes the format of a running one. `configure`
    /// runs with the clocks stopped, just before they start in `format`.
    pub(crate) fn open(&self, format: Format, configure: impl FnOnce(&StdFormat) -> Result<()>) -> Result<()> {
        let format = std_format(format);
        let mut tx = self.tx.lock();
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
            self.recreate_channel(&mut tx, format)
                .inspect_err(|err| error!(TAG, "i2s recreate: {err}"))?;
        }
        configure(&format)?;

        let tx = &mut *tx;
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

    /// Stops the clocks after `before`, which runs only while the stream is
    /// open. The clocks stop even when `before` fails.
    pub(crate) fn close(&self, before: impl FnOnce() -> Result<()>) -> Result<()> {
        let mut tx = self.tx.lock();
        if !tx.enabled {
            return Ok(());
        }
        let before = before();
        tx.enabled = false;
        self.enabled.store(false, Ordering::Release);
        let disable = tx.channel.as_ref().ok_or(Error::INVALID_STATE)?.disable();
        before.and(disable)
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
    // once it was sent, so everything queued before it has left the output, and
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

    /// The rate last opened.
    pub(crate) fn rate(&self) -> u32 {
        self.rate.load(Ordering::Relaxed)
    }
}
