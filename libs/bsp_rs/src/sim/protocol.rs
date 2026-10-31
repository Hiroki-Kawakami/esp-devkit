/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Events between the debug host and a simulator firmware; compiled into both
 * this crate and tools/devkit. See docs/simulator.md.
 */

pub const VERSION: u32 = 4;
pub const RECORD_SIZE: usize = 64;
pub const MAX_TOUCH_POINTS: usize = 10;
pub const MAX_NAME: usize = 48;

use core::ffi::CStr;

pub const ENV_SOCKET_FD: &CStr = c"SIM_HOST_FD";
pub const ENV_PSRAM_PATH: &CStr = c"SIM_PSRAM_PATH";
pub const ENV_PSRAM_SIZE: &CStr = c"SIM_PSRAM_SIZE";

/// Exit status of a firmware process asking to be started again.
pub const EXIT_RESTART: i32 = 75;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelFormat {
    L8,
    Rgb565,
    /// B, G, R in memory.
    Bgr888,
}

impl PixelFormat {
    pub const fn bytes(self) -> usize {
        match self {
            Self::L8 => 1,
            Self::Rgb565 => 2,
            Self::Bgr888 => 3,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct TouchPoint {
    pub x: u16,
    pub y: u16,
    pub id: u8,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Event {
    /// Firmware to host, once at start.
    Hello { version: u32 },
    /// Firmware to host: the board being simulated, NUL-padded UTF-8.
    Device { name: [u8; MAX_NAME] },
    /// Firmware to host: puts the frame at `offset` into the PSRAM region on
    /// the panel.
    Present { offset: u64, width: u16, height: u16, stride: u32, format: PixelFormat },
    /// Firmware to host: backlight 0..=100.
    Brightness { level: u8 },
    /// Host to firmware: every contact currently down, panel pixels.
    Touch { count: u8, points: [TouchPoint; MAX_TOUCH_POINTS] },
    /// Firmware to host: PCM stream `stream` starts in the ring at `offset`,
    /// replacing any other. Interleaved little-endian signed samples of 2 or 4
    /// bytes.
    AudioOpen { stream: u32, offset: u64, size: u32, sample_rate: u32, sample_bytes: u8, channels: u8 },
    /// Firmware to host: bytes written to the stream so far.
    AudioWrite { stream: u32, end: u64 },
    /// Firmware to host.
    AudioClose { stream: u32 },
    /// Firmware to host: the stream keeps playing, as silence. Outlives streams.
    AudioMute { mute: bool },
    /// Host to firmware: bytes of the stream played so far.
    AudioPlayed { stream: u32, bytes: u64 },
    /// Host to firmware: what the IMU reads from now on, in panel axes.
    Imu { accel: [f32; 3], gyro: [f32; 3] },
}

const TAG_HELLO: u8 = 1;
const TAG_PRESENT: u8 = 2;
const TAG_BRIGHTNESS: u8 = 3;
const TAG_TOUCH: u8 = 4;
const TAG_DEVICE: u8 = 5;
const TAG_AUDIO_OPEN: u8 = 6;
const TAG_AUDIO_WRITE: u8 = 7;
const TAG_AUDIO_CLOSE: u8 = 8;
const TAG_AUDIO_MUTE: u8 = 9;
const TAG_AUDIO_PLAYED: u8 = 10;
const TAG_IMU: u8 = 11;

struct Writer {
    buf: [u8; RECORD_SIZE],
    at: usize,
}

impl Writer {
    fn new(tag: u8) -> Self {
        let mut buf = [0; RECORD_SIZE];
        buf[0] = tag;
        Self { buf, at: 1 }
    }

    fn put(&mut self, bytes: &[u8]) -> &mut Self {
        self.buf[self.at..self.at + bytes.len()].copy_from_slice(bytes);
        self.at += bytes.len();
        self
    }
}

struct Reader<'a> {
    buf: &'a [u8; RECORD_SIZE],
    at: usize,
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0; N];
        out.copy_from_slice(&self.buf[self.at..self.at + N]);
        self.at += N;
        out
    }

    fn u8(&mut self) -> u8 {
        self.take::<1>()[0]
    }

    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take())
    }

    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }

    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.take())
    }

    fn vec3(&mut self) -> [f32; 3] {
        [(); 3].map(|_| f32::from_le_bytes(self.take()))
    }
}

impl Event {
    /// Truncated to `MAX_NAME` bytes.
    pub fn device(name: &str) -> Self {
        let mut buf = [0; MAX_NAME];
        let n = name.len().min(MAX_NAME);
        buf[..n].copy_from_slice(&name.as_bytes()[..n]);
        Self::Device { name: buf }
    }

    pub fn encode(&self) -> [u8; RECORD_SIZE] {
        match *self {
            Self::Hello { version } => Writer::new(TAG_HELLO).put(&version.to_le_bytes()).buf,
            Self::Present { offset, width, height, stride, format } => Writer::new(TAG_PRESENT)
                .put(&offset.to_le_bytes())
                .put(&width.to_le_bytes())
                .put(&height.to_le_bytes())
                .put(&stride.to_le_bytes())
                .put(&[format as u8])
                .buf,
            Self::Brightness { level } => Writer::new(TAG_BRIGHTNESS).put(&[level]).buf,
            Self::Device { name } => Writer::new(TAG_DEVICE).put(&name).buf,
            Self::Touch { count, points } => {
                let mut w = Writer::new(TAG_TOUCH);
                w.put(&[count]);
                for p in &points {
                    w.put(&p.x.to_le_bytes()).put(&p.y.to_le_bytes()).put(&[p.id]);
                }
                w.buf
            }
            Self::AudioOpen { stream, offset, size, sample_rate, sample_bytes, channels } => {
                Writer::new(TAG_AUDIO_OPEN)
                    .put(&stream.to_le_bytes())
                    .put(&offset.to_le_bytes())
                    .put(&size.to_le_bytes())
                    .put(&sample_rate.to_le_bytes())
                    .put(&[sample_bytes, channels])
                    .buf
            }
            Self::AudioWrite { stream, end } => {
                Writer::new(TAG_AUDIO_WRITE).put(&stream.to_le_bytes()).put(&end.to_le_bytes()).buf
            }
            Self::AudioClose { stream } => Writer::new(TAG_AUDIO_CLOSE).put(&stream.to_le_bytes()).buf,
            Self::AudioMute { mute } => Writer::new(TAG_AUDIO_MUTE).put(&[u8::from(mute)]).buf,
            Self::AudioPlayed { stream, bytes } => {
                Writer::new(TAG_AUDIO_PLAYED).put(&stream.to_le_bytes()).put(&bytes.to_le_bytes()).buf
            }
            Self::Imu { accel, gyro } => {
                let mut w = Writer::new(TAG_IMU);
                for v in accel.iter().chain(&gyro) {
                    w.put(&v.to_le_bytes());
                }
                w.buf
            }
        }
    }

    pub fn decode(buf: &[u8; RECORD_SIZE]) -> Option<Self> {
        let mut r = Reader { buf, at: 1 };
        Some(match buf[0] {
            TAG_HELLO => Self::Hello { version: r.u32() },
            TAG_PRESENT => Self::Present {
                offset: r.u64(),
                width: r.u16(),
                height: r.u16(),
                stride: r.u32(),
                format: match r.u8() {
                    0 => PixelFormat::L8,
                    1 => PixelFormat::Rgb565,
                    2 => PixelFormat::Bgr888,
                    _ => return None,
                },
            },
            TAG_BRIGHTNESS => Self::Brightness { level: r.u8() },
            TAG_DEVICE => Self::Device { name: r.take() },
            TAG_TOUCH => {
                let count = r.u8().min(MAX_TOUCH_POINTS as u8);
                let mut points = [TouchPoint::default(); MAX_TOUCH_POINTS];
                for p in &mut points {
                    *p = TouchPoint { x: r.u16(), y: r.u16(), id: r.u8() };
                }
                Self::Touch { count, points }
            }
            TAG_AUDIO_OPEN => Self::AudioOpen {
                stream: r.u32(),
                offset: r.u64(),
                size: r.u32(),
                sample_rate: r.u32(),
                sample_bytes: r.u8(),
                channels: r.u8(),
            },
            TAG_AUDIO_WRITE => Self::AudioWrite { stream: r.u32(), end: r.u64() },
            TAG_AUDIO_CLOSE => Self::AudioClose { stream: r.u32() },
            TAG_AUDIO_MUTE => Self::AudioMute { mute: r.u8() != 0 },
            TAG_AUDIO_PLAYED => Self::AudioPlayed { stream: r.u32(), bytes: r.u64() },
            TAG_IMU => Self::Imu { accel: r.vec3(), gyro: r.vec3() },
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Event, MAX_TOUCH_POINTS, PixelFormat, TouchPoint};

    #[test]
    fn events_round_trip() {
        let mut points = [TouchPoint::default(); MAX_TOUCH_POINTS];
        points[0] = TouchPoint { x: 719, y: 1279, id: 3 };
        points[9] = TouchPoint { x: 1, y: 2, id: 9 };
        for e in [
            Event::Hello { version: 1 },
            Event::Present { offset: 0x1_2345_6789, width: 720, height: 1280, stride: 1440, format: PixelFormat::Rgb565 },
            Event::Brightness { level: 100 },
            Event::device("M5Stack Tab5"),
            Event::Touch { count: 10, points },
            Event::AudioOpen { stream: 7, offset: 0x1_0000_0000, size: 1024, sample_rate: 48_000, sample_bytes: 4, channels: 2 },
            Event::AudioWrite { stream: 7, end: 0x1_0000_0001 },
            Event::AudioClose { stream: 7 },
            Event::AudioMute { mute: true },
            Event::AudioPlayed { stream: 7, bytes: 0x1_0000_0002 },
            Event::Imu { accel: [0.0, -1.0, 0.0], gyro: [1.5, -2.5, 3.5] },
        ] {
            assert_eq!(Event::decode(&e.encode()), Some(e));
        }
        assert_eq!(Event::decode(&[0; super::RECORD_SIZE]), None);
    }
}
