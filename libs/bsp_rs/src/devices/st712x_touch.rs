/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::time::Duration;

use idf::i2c::{Device, MasterBus};
use idf::{Result, gpio};

use crate::touch::{Driver, Point};

pub(crate) const ADDR: u16 = 0x55;
pub(crate) const DEFAULT_HZ: u32 = 100_000;
const MAX_POINTS: usize = 10;

const REG_MAX_TOUCHES: u16 = 0x0009;
const REG_SENSING_COUNTER: u16 = 0x000A;
const REG_ADV_INFO: u16 = 0x0010;
const REG_REPORT_COORD: u16 = 0x0014;
const ADV_INFO_WITH_COORD: u8 = 0x08;
const POINT_SIZE: usize = 7;
const COORD_OFFSET: usize = (REG_REPORT_COORD - REG_SENSING_COUNTER) as usize;
const I2C_TIMEOUT: Duration = Duration::from_millis(50);
const TRACK_REACH: i32 = 200;

pub(crate) struct Config {
    pub clock_hz: u32,
    pub int_pin: Option<i32>,
}

#[derive(Clone, Copy, Default)]
struct Track {
    at: (i32, i32),
    per_frame: (i32, i32),
    id: u8,
}

/// Gives the untracked points of a report ids: each continues the finger of
/// the last report it is nearest to, where that finger was or would be by now,
/// within reach, or takes a new id.
struct Tracks {
    last: [Track; MAX_POINTS],
    count: usize,
    frame: u16,
    next_id: u8,
}

impl Tracks {
    const fn new() -> Self {
        Self { last: [Track { at: (0, 0), per_frame: (0, 0), id: 0 }; MAX_POINTS], count: 0, frame: 0, next_id: 0 }
    }

    /// `frame` is the chip's scan counter at the report; the same frame again
    /// keeps the motion.
    fn assign(&mut self, points: &mut [Point], frame: u16) {
        let frames = i32::from(frame.wrapping_sub(self.frame));
        self.frame = frame;
        let last = &self.last[..self.count];
        let distance = |a: (i64, i64), b: (i64, i64)| (a.0 - b.0).pow(2) + (a.1 - b.1).pow(2);
        let mut pairs = [(0i64, 0u8, 0u8); MAX_POINTS * MAX_POINTS];
        let mut n = 0;
        for (p, point) in points.iter().enumerate() {
            let at = (i64::from(point.x), i64::from(point.y));
            for (t, track) in last.iter().enumerate() {
                let from = (i64::from(track.at.0), i64::from(track.at.1));
                let step = (i64::from(track.per_frame.0) * i64::from(frames), i64::from(track.per_frame.1) * i64::from(frames));
                let ahead = (from.0 + step.0, from.1 + step.1);
                // Where it was counts too: a prediction overshoots a finger that stopped.
                let cost = distance(at, ahead).min(distance(at, from));
                // A flick still speeding up outruns its prediction by about the step itself.
                let reach = i64::from(TRACK_REACH) + (distance(step, (0, 0)) as u64).isqrt() as i64;
                if cost <= reach * reach {
                    pairs[n] = (cost, p as u8, t as u8);
                    n += 1;
                }
            }
        }
        pairs[..n].sort_unstable_by_key(|pair| pair.0);
        let mut next = [Track::default(); MAX_POINTS];
        let mut matched = [false; MAX_POINTS];
        let mut taken = [false; MAX_POINTS];
        for &(_, p, t) in &pairs[..n] {
            let (p, t) = (usize::from(p), usize::from(t));
            if matched[p] || taken[t] {
                continue;
            }
            (matched[p], taken[t]) = (true, true);
            let at = (i32::from(points[p].x), i32::from(points[p].y));
            let from = last[t].at;
            let per_frame = if frames > 0 { ((at.0 - from.0) / frames, (at.1 - from.1) / frames) } else { last[t].per_frame };
            next[p] = Track { at, per_frame, id: last[t].id };
        }
        for p in 0..points.len() {
            if !matched[p] {
                // Past every id of the last report, so a new finger is never read
                // as a lifted one that jumped.
                while last.iter().any(|t| t.id == self.next_id) {
                    self.next_id = self.next_id.wrapping_add(1);
                }
                next[p] = Track { at: (i32::from(points[p].x), i32::from(points[p].y)), per_frame: (0, 0), id: self.next_id };
                self.next_id = self.next_id.wrapping_add(1);
            }
            points[p].id = next[p].id;
        }
        self.last = next;
        self.count = points.len();
    }
}

pub(crate) struct St712xTouch {
    dev: Device<'static>,
    points: [Point; MAX_POINTS],
    count: usize,
    tracks: Tracks,
}

impl St712xTouch {
    /// The board resets the chip beforehand.
    pub(crate) fn new(bus: &'static MasterBus, config: &Config) -> Result<Self> {
        if let Some(pin) = config.int_pin {
            gpio::set_direction(pin, gpio::Mode::Input)?;
            gpio::set_pull(pin, gpio::Pull::Up)?;
        }
        Ok(Self { dev: bus.add_device(ADDR, config.clock_hz)?, points: [Point::default(); MAX_POINTS], count: 0, tracks: Tracks::new() })
    }

    fn read_reg(&self, reg: u16, buf: &mut [u8]) -> Result<()> {
        self.dev.transmit_receive(&reg.to_be_bytes(), buf, I2C_TIMEOUT)
    }
}

impl Driver for St712xTouch {
    fn poll(&mut self) -> Result<&[Point]> {
        let mut adv = [0u8];
        self.read_reg(REG_ADV_INFO, &mut adv)?;
        if adv[0] & ADV_INFO_WITH_COORD != 0 {
            let mut max = [0u8];
            self.read_reg(REG_MAX_TOUCHES, &mut max)?;
            let reports = usize::from(max[0]).min(MAX_POINTS);
            // From the scan counter on, so the counter is of the same report.
            let mut raw = [0u8; COORD_OFFSET + MAX_POINTS * POINT_SIZE];
            self.read_reg(REG_SENSING_COUNTER, &mut raw[..COORD_OFFSET + reports * POINT_SIZE])?;
            let frame = u16::from_be_bytes([raw[0], raw[1]]);
            let valid = raw[COORD_OFFSET..].chunks_exact(POINT_SIZE).take(reports).filter(|r| r[0] & 0x80 != 0);
            self.count = 0;
            for r in valid {
                self.points[self.count] = Point {
                    x: u16::from(r[0] & 0x3F) << 8 | u16::from(r[1]),
                    y: u16::from(r[2]) << 8 | u16::from(r[3]),
                    id: 0,
                };
                self.count += 1;
            }
            // Reports carry no track id.
            self.tracks.assign(&mut self.points[..self.count], frame);
        }
        Ok(&self.points[..self.count])
    }
}
