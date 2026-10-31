/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::ffi::CStr;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use core::time::Duration;

use idf::heap::{self, Caps};
use idf::log::error;
use idf::sync::{Mutex, Semaphore};
use idf::{Error, Result};

use super::Host;
use super::protocol::Event;
use crate::audio::{Driver, Format, HwMute};

const TAG: &CStr = c"sim_audio";

const RING_BYTES: usize = 128 * 1024;
// About the depth of a device's DMA ring, so a writer paces as it would there.
const HIGH_WATER: Duration = Duration::from_millis(100);
const STALL_TIMEOUT: Duration = Duration::from_secs(2);

struct Progress {
    stream: AtomicU32,
    played: AtomicU64,
    wake: Semaphore,
}

struct Tx {
    ring: Box<[u8], Caps>,
    offset: u64,
    open: bool,
    frame_bytes: usize,
    high_water: u64,
    written: u64,
}

pub(crate) struct SimAudio {
    host: &'static Host,
    progress: Arc<Progress>,
    tx: Mutex<Tx>,
    muted: AtomicBool,
    speaker: AtomicBool,
}

impl SimAudio {
    pub(crate) fn new(host: &'static Host) -> Result<Self> {
        let ring = heap::zeroed_slice(RING_BYTES, Caps::SPIRAM)?;
        let offset = host.psram_offset(ring.as_ptr()).ok_or(Error::INVALID_STATE)?;
        let progress = Arc::new(Progress {
            stream: AtomicU32::new(0),
            played: AtomicU64::new(0),
            wake: Semaphore::try_new(0, 1)?,
        });
        let listener = progress.clone();
        host.set_audio_listener(Box::new(move |stream, bytes| {
            if stream == listener.stream.load(Ordering::Acquire) {
                listener.played.store(bytes, Ordering::Release);
                listener.wake.give();
            }
        }));
        Ok(Self {
            host,
            progress,
            tx: Mutex::try_new(Tx { ring, offset, open: false, frame_bytes: 0, high_water: 0, written: 0 })?,
            muted: AtomicBool::new(true),
            speaker: AtomicBool::new(false),
        })
    }

    fn stream(&self) -> u32 {
        self.progress.stream.load(Ordering::Acquire)
    }

    fn pending(&self, tx: &Tx) -> u64 {
        tx.written.saturating_sub(self.progress.played.load(Ordering::Acquire))
    }

    // Consumption goes on underneath, as it does behind a real mute or amp.
    fn apply_silence(&self) {
        let mute = self.muted.load(Ordering::Relaxed) || !self.speaker.load(Ordering::Relaxed);
        self.host.send(&Event::AudioMute { mute });
    }
}

impl Driver for SimAudio {
    fn open(&self, format: Format) -> Result<()> {
        let sample_bytes = format.sample.bytes();
        let frame_bytes = usize::from(format.channels) * sample_bytes;
        let mut tx = self.tx.lock();
        let stream = self.stream().wrapping_add(1);
        self.progress.played.store(0, Ordering::Release);
        self.progress.stream.store(stream, Ordering::Release);
        let frames = u64::from(format.sample_rate) * HIGH_WATER.as_millis() as u64 / 1000;
        let ring_frames = (tx.ring.len() / frame_bytes) as u64;
        tx.open = true;
        tx.frame_bytes = frame_bytes;
        tx.high_water = frames.clamp(1, ring_frames) * frame_bytes as u64;
        tx.written = 0;
        self.host.send(&Event::AudioOpen {
            stream,
            offset: tx.offset,
            size: tx.ring.len() as u32,
            sample_rate: format.sample_rate,
            sample_bytes: sample_bytes as u8,
            channels: format.channels,
        });
        Ok(())
    }

    fn close(&self) -> Result<()> {
        let mut tx = self.tx.lock();
        if tx.open {
            tx.open = false;
            self.host.send(&Event::AudioClose { stream: self.stream() });
        }
        Ok(())
    }

    fn write(&self, data: &[u8]) -> Result<()> {
        let mut tx = self.tx.lock();
        if !tx.open {
            return Err(Error::INVALID_STATE);
        }
        let mut rest = data;
        while !rest.is_empty() {
            let used = self.pending(&tx);
            if used >= tx.high_water {
                if !self.progress.wake.take_timeout(STALL_TIMEOUT) {
                    error!(TAG, "host stopped playing");
                    return Err(Error::TIMEOUT);
                }
                continue;
            }
            let at = (tx.written % tx.ring.len() as u64) as usize;
            let n = rest.len().min((tx.high_water - used) as usize).min(tx.ring.len() - at);
            tx.ring[at..at + n].copy_from_slice(&rest[..n]);
            tx.written += n as u64;
            rest = &rest[n..];
            self.host.send(&Event::AudioWrite { stream: self.stream(), end: tx.written });
        }
        Ok(())
    }

    fn drain(&self) -> Result<()> {
        let tx = self.tx.lock();
        while tx.open && self.pending(&tx) > 0 {
            if !self.progress.wake.take_timeout(STALL_TIMEOUT) {
                error!(TAG, "host stopped playing");
                return Err(Error::TIMEOUT);
            }
        }
        Ok(())
    }

    fn queued(&self) -> Result<usize> {
        let tx = self.tx.lock();
        if !tx.open {
            return Err(Error::INVALID_STATE);
        }
        Ok(self.pending(&tx) as usize / tx.frame_bytes)
    }

    fn hw_mute(&self) -> HwMute {
        HwMute::Step
    }

    fn set_hw_mute(&self, mute: bool) -> Result<()> {
        self.muted.store(mute, Ordering::Relaxed);
        self.apply_silence();
        Ok(())
    }

    fn has_speaker(&self) -> bool {
        true
    }

    fn set_speaker_enabled(&self, enabled: bool) -> Result<()> {
        self.speaker.store(enabled, Ordering::Relaxed);
        self.apply_silence();
        Ok(())
    }
}
