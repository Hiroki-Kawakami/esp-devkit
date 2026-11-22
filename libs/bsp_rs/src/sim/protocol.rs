/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Events between the debug host and a simulator firmware; compiled into both
 * this crate and tools/devkit_cli. See docs/simulator.md.
 */

pub const VERSION: u32 = 8;
pub const RECORD_SIZE: usize = 64;
pub const MAX_TOUCH_POINTS: usize = 10;
pub const MAX_NAME: usize = 48;

use core::ffi::CStr;

pub const ENV_SOCKET_FD: &CStr = c"SIM_HOST_FD";
pub const ENV_PSRAM_PATH: &CStr = c"SIM_PSRAM_PATH";
pub const ENV_PSRAM_SIZE: &CStr = c"SIM_PSRAM_SIZE";
/// The `Power` the firmware starts with, as `<level> <external> <battery>`
/// with 0 or 1 for the flags.
pub const ENV_POWER: &CStr = c"SIM_POWER";

/// The clock the firmware starts with, as `<seconds> <valid>`: seconds since
/// 1970 UTC and 0 or 1.
pub const ENV_RTC: &CStr = c"SIM_RTC";

pub fn parse_rtc(value: &str) -> Option<(i64, bool)> {
    let mut fields = value.split_whitespace();
    let seconds = fields.next()?.parse().ok()?;
    let valid: u8 = fields.next()?.parse().ok()?;
    Some((seconds, valid != 0))
}

pub fn parse_power(value: &str) -> Option<(u8, bool, bool)> {
    let mut fields = value.split_whitespace();
    let mut next = || fields.next()?.parse::<u8>().ok();
    let (level, external, battery) = (next()?, next()?, next()?);
    (level <= 100).then_some((level, external != 0, battery != 0))
}

/// Exit status of a firmware process asking to be started again.
pub const EXIT_RESTART: i32 = 75;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelFormat {
    L8,
    Rgb565,
    /// B, G, R in memory.
    Bgr888,
    Rgb565Be,
    /// Two pixels a byte, the first in the high nibble.
    L4,
}

impl PixelFormat {
    pub const fn bits(self) -> usize {
        match self {
            Self::L4 => 4,
            Self::L8 => 8,
            Self::Rgb565 | Self::Rgb565Be => 16,
            Self::Bgr888 => 24,
        }
    }

    /// A row of `width` starts on a byte.
    pub const fn row_bytes(self, width: usize) -> usize {
        (width * self.bits()).div_ceil(8)
    }

    const fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::L8,
            1 => Self::Rgb565,
            2 => Self::Bgr888,
            3 => Self::Rgb565Be,
            4 => Self::L4,
            _ => return None,
        })
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
    /// Host to firmware: battery level 0..=100, external power, battery in.
    Power { level: u8, external: bool, battery: bool },
    /// Firmware to host: the clock was set, to seconds since 1970 UTC.
    Rtc { seconds: i64, valid: bool },
    /// Firmware to host: a panel with memory of its own, `width` by `height`
    /// unturned, that takes `bytes_per_second`. `rotation` in quarter turns
    /// clockwise.
    GramOpen { width: u16, height: u16, format: PixelFormat, rotation: u8, bytes_per_second: u32 },
    /// Firmware to host: writes after it land turned by `rotation`.
    GramRotate { rotation: u8 },
    /// Firmware to host: writes after it come in `format`; the panel keeps
    /// what it shows.
    GramFormat { format: PixelFormat },
    /// Firmware to host: `bytes` of pixels follow the record, the rows of
    /// `x, y, w, h` of the turned picture back to back.
    GramWrite { ticket: u32, x: u16, y: u16, w: u16, h: u16, bytes: u32 },
    /// Host to firmware: every write up to `ticket` has been sent.
    GramDone { ticket: u32 },
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
const TAG_POWER: u8 = 12;
const TAG_RTC: u8 = 13;
const TAG_GRAM_OPEN: u8 = 14;
const TAG_GRAM_ROTATE: u8 = 15;
const TAG_GRAM_WRITE: u8 = 16;
const TAG_GRAM_DONE: u8 = 17;
const TAG_GRAM_FORMAT: u8 = 18;

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
            Self::Power { level, external, battery } => {
                Writer::new(TAG_POWER).put(&[level, u8::from(external), u8::from(battery)]).buf
            }
            Self::Rtc { seconds, valid } => Writer::new(TAG_RTC).put(&seconds.to_le_bytes()).put(&[u8::from(valid)]).buf,
            Self::GramOpen { width, height, format, rotation, bytes_per_second } => Writer::new(TAG_GRAM_OPEN)
                .put(&width.to_le_bytes())
                .put(&height.to_le_bytes())
                .put(&[format as u8, rotation])
                .put(&bytes_per_second.to_le_bytes())
                .buf,
            Self::GramRotate { rotation } => Writer::new(TAG_GRAM_ROTATE).put(&[rotation]).buf,
            Self::GramFormat { format } => Writer::new(TAG_GRAM_FORMAT).put(&[format as u8]).buf,
            Self::GramWrite { ticket, x, y, w, h, bytes } => Writer::new(TAG_GRAM_WRITE)
                .put(&ticket.to_le_bytes())
                .put(&x.to_le_bytes())
                .put(&y.to_le_bytes())
                .put(&w.to_le_bytes())
                .put(&h.to_le_bytes())
                .put(&bytes.to_le_bytes())
                .buf,
            Self::GramDone { ticket } => Writer::new(TAG_GRAM_DONE).put(&ticket.to_le_bytes()).buf,
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
                format: PixelFormat::from_u8(r.u8())?,
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
            TAG_RTC => Self::Rtc { seconds: r.u64() as i64, valid: r.u8() != 0 },
            TAG_POWER => Self::Power { level: r.u8(), external: r.u8() != 0, battery: r.u8() != 0 },
            TAG_GRAM_OPEN => Self::GramOpen {
                width: r.u16(),
                height: r.u16(),
                format: PixelFormat::from_u8(r.u8())?,
                rotation: r.u8(),
                bytes_per_second: r.u32(),
            },
            TAG_GRAM_ROTATE => Self::GramRotate { rotation: r.u8() },
            TAG_GRAM_FORMAT => Self::GramFormat { format: PixelFormat::from_u8(r.u8())? },
            TAG_GRAM_WRITE => Self::GramWrite { ticket: r.u32(), x: r.u16(), y: r.u16(), w: r.u16(), h: r.u16(), bytes: r.u32() },
            TAG_GRAM_DONE => Self::GramDone { ticket: r.u32() },
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
            Event::Power { level: 76, external: true, battery: false },
            Event::Rtc { seconds: -1, valid: true },
            Event::GramOpen { width: 320, height: 240, format: PixelFormat::Rgb565Be, rotation: 3, bytes_per_second: 5_000_000 },
            Event::GramRotate { rotation: 1 },
            Event::GramFormat { format: PixelFormat::L4 },
            Event::GramWrite { ticket: 0xFFFF_FFFE, x: 1, y: 2, w: 319, h: 16, bytes: 10_208 },
            Event::GramDone { ticket: 7 },
        ] {
            assert_eq!(Event::decode(&e.encode()), Some(e));
        }
        assert_eq!(Event::decode(&[0; super::RECORD_SIZE]), None);
        assert_eq!(super::parse_power("76 1 0"), Some((76, true, false)));
        assert_eq!(super::parse_power("101 1 1"), None);
        assert_eq!(super::parse_power("76 1"), None);
        assert_eq!(super::parse_rtc("1700000000 0"), Some((1_700_000_000, false)));
    }
}
