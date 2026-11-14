/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Plays a firmware's PCM ring out of the shared PSRAM region. Played bytes go
 * back to the firmware, whose writes pace on them.
 */

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, Stream};

const TICK: Duration = Duration::from_millis(5);

#[derive(Clone, Copy)]
pub struct Ring {
    pub ptr: *const u8,
    pub size: u64,
}

unsafe impl Send for Ring {}

#[derive(Clone, Copy)]
pub struct Format {
    pub sample_rate: u32,
    pub sample_bytes: u8,
    pub channels: u8,
}

impl Format {
    fn frame_bytes(&self) -> u64 {
        u64::from(self.sample_bytes) * u64::from(self.channels)
    }
}

struct Shared {
    written: AtomicU64,
    played: AtomicU64,
    muted: AtomicBool,
    stop: AtomicBool,
}

/// Dropping it stops the playback.
pub struct Player {
    pub stream: u32,
    shared: Arc<Shared>,
}

impl Player {
    /// `silent` keeps time without opening an audio device, which is also the
    /// fallback when none opens.
    pub fn start(
        stream: u32,
        ring: Ring,
        format: Format,
        muted: bool,
        silent: bool,
        report: impl Fn(u64) + Send + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            written: AtomicU64::new(0),
            played: AtomicU64::new(0),
            muted: AtomicBool::new(muted),
            stop: AtomicBool::new(false),
        });
        let worker = shared.clone();
        thread::spawn(move || run(&worker, ring, format, silent, report));
        Self { stream, shared }
    }

    pub fn write(&self, end: u64) {
        self.shared.written.store(end, Ordering::Release);
    }

    pub fn set_muted(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

// The cpal stream is not Send, so it lives and ends on this thread.
fn run(shared: &Arc<Shared>, ring: Ring, format: Format, silent: bool, report: impl Fn(u64)) {
    let output = if silent {
        None
    } else {
        open_output(shared, ring, format)
            .inspect_err(|err| eprintln!("[devkit] no audio output: {err}"))
            .ok()
    };
    let frame_bytes = format.frame_bytes();
    let bytes_per_second = f64::from(format.sample_rate) * frame_bytes as f64;
    let mut credit = 0.0;
    let mut last = Instant::now();
    let mut reported = 0;
    while !shared.stop.load(Ordering::Relaxed) {
        thread::sleep(TICK);
        if output.is_none() {
            let now = Instant::now();
            credit += now.duration_since(last).as_secs_f64() * bytes_per_second;
            last = now;
            let played = shared.played.load(Ordering::Relaxed);
            let waiting = shared.written.load(Ordering::Acquire).saturating_sub(played);
            let whole = (credit as u64 / frame_bytes * frame_bytes).min(waiting);
            shared.played.store(played + whole, Ordering::Relaxed);
            credit = if waiting == 0 { 0.0 } else { credit - whole as f64 };
        }
        let played = shared.played.load(Ordering::Relaxed);
        if played != reported {
            reported = played;
            report(played);
        }
    }
}

fn open_output(shared: &Arc<Shared>, ring: Ring, format: Format) -> Result<Stream, String> {
    let device = cpal::default_host().default_output_device().ok_or("no default device")?;
    let config = device.default_output_config().map_err(|err| err.to_string())?;
    let reader = Reader::new(shared.clone(), ring, format, config.sample_rate());
    let channels = usize::from(config.channels());
    let stream = match config.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, config.config(), reader, channels),
        SampleFormat::I32 => build::<i32>(&device, config.config(), reader, channels),
        SampleFormat::I16 => build::<i16>(&device, config.config(), reader, channels),
        SampleFormat::U16 => build::<u16>(&device, config.config(), reader, channels),
        other => return Err(format!("unsupported device format {other}")),
    }?;
    stream.play().map_err(|err| err.to_string())?;
    Ok(stream)
}

fn build<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut reader: Reader,
    channels: usize,
) -> Result<Stream, String> {
    device
        .build_output_stream(
            config,
            move |out: &mut [T], _| {
                for frame in out.chunks_exact_mut(channels) {
                    let (left, right) = reader.next();
                    if let [mono] = frame {
                        *mono = T::from_sample((left + right) / 2.0);
                        continue;
                    }
                    for (i, sample) in frame.iter_mut().enumerate() {
                        let value = match i {
                            0 => left,
                            1 => right,
                            _ => 0.0,
                        };
                        *sample = T::from_sample(value);
                    }
                }
            },
            |err| eprintln!("[devkit] audio output: {err}"),
            None,
        )
        .map_err(|err| err.to_string())
}

// Runs in the device's callback, so it only touches atomics and the ring. The
// device stays at its own rate; the stream is resampled linearly to it, which
// is plenty for a simulator.
struct Reader {
    shared: Arc<Shared>,
    ring: Ring,
    format: Format,
    step: f64,
    phase: f64,
    current: (f32, f32),
    next: (f32, f32),
}

impl Reader {
    fn new(shared: Arc<Shared>, ring: Ring, format: Format, device_rate: u32) -> Self {
        let step = f64::from(format.sample_rate) / f64::from(device_rate);
        Self { shared, ring, format, step, phase: 1.0, current: (0.0, 0.0), next: (0.0, 0.0) }
    }

    fn next(&mut self) -> (f32, f32) {
        while self.phase >= 1.0 {
            self.current = self.next;
            self.next = self.pop();
            self.phase -= 1.0;
        }
        let t = self.phase as f32;
        self.phase += self.step;
        (
            self.current.0 + (self.next.0 - self.current.0) * t,
            self.current.1 + (self.next.1 - self.current.1) * t,
        )
    }

    fn pop(&mut self) -> (f32, f32) {
        let played = self.shared.played.load(Ordering::Relaxed);
        let frame_bytes = self.format.frame_bytes();
        if played + frame_bytes > self.shared.written.load(Ordering::Acquire) {
            return (0.0, 0.0);
        }
        self.shared.played.store(played + frame_bytes, Ordering::Relaxed);
        if self.shared.muted.load(Ordering::Relaxed) {
            return (0.0, 0.0);
        }
        let left = self.sample(played);
        if self.format.channels == 1 {
            return (left, left);
        }
        (left, self.sample(played + u64::from(self.format.sample_bytes)))
    }

    fn sample(&self, at: u64) -> f32 {
        let byte = |i: u64| unsafe { *self.ring.ptr.add(((at + i) % self.ring.size) as usize) };
        if self.format.sample_bytes == 2 {
            f32::from(i16::from_le_bytes([byte(0), byte(1)])) / 32768.0
        } else {
            i32::from_le_bytes([byte(0), byte(1), byte(2), byte(3)]) as f32 / 2147483648.0
        }
    }
}

unsafe impl Send for Reader {}

#[cfg(test)]
mod tests {
    use super::*;

    fn reader(bytes: &[u8], format: Format, device_rate: u32) -> (Reader, Arc<Shared>) {
        let shared = Arc::new(Shared {
            written: AtomicU64::new(bytes.len() as u64),
            played: AtomicU64::new(0),
            muted: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let ring = Ring { ptr: bytes.as_ptr(), size: bytes.len() as u64 };
        (Reader::new(shared.clone(), ring, format, device_rate), shared)
    }

    fn s16(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    const STEREO: Format = Format { sample_rate: 48_000, sample_bytes: 2, channels: 2 };

    #[test]
    fn reads_stereo_frames_and_goes_silent_when_dry() {
        let bytes = s16(&[16384, -16384]);
        let (mut reader, shared) = reader(&bytes, STEREO, 48_000);
        assert_eq!(reader.next(), (0.0, 0.0));
        assert_eq!(reader.next(), (0.5, -0.5));
        assert_eq!(reader.next(), (0.0, 0.0));
        assert_eq!(shared.played.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn mono_goes_to_both_sides() {
        let bytes = s16(&[16384]);
        let format = Format { channels: 1, ..STEREO };
        let (mut reader, _) = reader(&bytes, format, 48_000);
        reader.next();
        assert_eq!(reader.next(), (0.5, 0.5));
    }

    #[test]
    fn s32_is_full_range() {
        let bytes: Vec<u8> = [1i32 << 30, -(1i32 << 30)].iter().flat_map(|s| s.to_le_bytes()).collect();
        let format = Format { sample_bytes: 4, ..STEREO };
        let (mut reader, _) = reader(&bytes, format, 48_000);
        reader.next();
        assert_eq!(reader.next(), (0.5, -0.5));
    }

    #[test]
    fn muted_frames_are_consumed_as_silence() {
        let bytes = s16(&[16384, 16384, 16384, 16384]);
        let (mut reader, shared) = reader(&bytes, STEREO, 48_000);
        shared.muted.store(true, Ordering::Relaxed);
        reader.next();
        assert_eq!(reader.next(), (0.0, 0.0));
        assert_eq!(shared.played.load(Ordering::Relaxed), 8);
    }

    #[test]
    fn samples_wrap_around_the_ring() {
        let bytes = s16(&[16384, -16384]);
        let (mut reader, shared) = reader(&bytes, STEREO, 48_000);
        shared.played.store(2, Ordering::Relaxed);
        shared.written.store(6, Ordering::Release);
        reader.next();
        assert_eq!(reader.next(), (-0.5, 0.5));
    }

    #[test]
    fn resamples_to_the_device_rate() {
        let bytes = s16(&[0, 0, 16384, 16384]);
        let (mut reader, shared) = reader(&bytes, STEREO, 96_000);
        reader.next();
        reader.next();
        assert_eq!(reader.next(), (0.0, 0.0));
        assert_eq!(reader.next(), (0.25, 0.25));
        assert_eq!(reader.next(), (0.5, 0.5));
        assert_eq!(shared.played.load(Ordering::Relaxed), 8);
    }

    #[test]
    fn silent_player_paces_at_the_stream_rate() {
        let ring = vec![0u8; 4800];
        let (sender, receiver) = std::sync::mpsc::channel();
        let player = Player::start(
            1,
            Ring { ptr: ring.as_ptr(), size: 4800 },
            STEREO,
            false,
            true,
            move |played| {
                let _ = sender.send(played);
            },
        );
        player.write(4800);
        let start = Instant::now();
        while receiver.recv_timeout(Duration::from_secs(2)).unwrap() < 4800 {}
        assert!(start.elapsed() >= Duration::from_millis(15));
    }
}
