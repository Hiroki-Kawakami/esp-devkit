/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::f32::consts::PI;
use core::marker::PhantomData;
use core::slice;
use core::sync::atomic::{AtomicI32, Ordering};

use devkit::math::{ceilf, lrintf, sinf, sqrtf};
use idf::heap::{self, Caps};

use crate::sample::{Format, check_channels};
use crate::{Error, Result, Sample};

pub const MAX_ADJUST_PPM: f32 = 10000.0;

const DEFAULT_MAX_IN: usize = 1024;
const DEFAULT_TAPS: usize = 48;
const DEFAULT_PHASES: u32 = 64;
const MAX_PHASES: u32 = 1024;
const COEFF_Q: u32 = 30;
const COEFF_HALF: i64 = 1 << (COEFF_Q - 1);
const MU_BITS: u32 = 16;
const KAISER_BETA: f32 = 7.0;
const PASSBAND: f32 = 0.88;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ResamplerKind {
    /// Catmull-Rom, no anti-aliasing: ratios near 1.
    #[default]
    Cubic,
    /// Windowed-sinc, any ratio.
    Polyphase,
    /// Windowed-sinc, one rate a multiple of the other.
    Integer,
}

#[derive(Clone, Copy, Debug)]
pub struct ResamplerConfig {
    pub kind: ResamplerKind,
    pub channels: u8,
    pub in_rate: u32,
    pub out_rate: u32,
    /// 0 = 1024.
    pub max_in_frames: usize,
    /// Sinc taps per output at the higher rate; 0 = 48.
    pub taps: u16,
    /// `Polyphase` only, a power of two up to 1024; 0 = 64.
    pub phases: u16,
    pub caps: Caps,
}

impl Default for ResamplerConfig {
    fn default() -> Self {
        Self {
            kind: ResamplerKind::Cubic,
            channels: 2,
            in_rate: 48000,
            out_rate: 48000,
            max_in_frames: 0,
            taps: 0,
            phases: 0,
            caps: Caps::DEFAULT,
        }
    }
}

/// Sample-rate converter. The ratio trim goes through [`ResamplerControl`].
pub struct Resampler<S: Sample> {
    core: Core,
    _sample: PhantomData<S>,
}

struct Core {
    kind: ResamplerKind,
    channels: usize,
    in_rate: u32,
    out_rate: u32,
    max_in: usize,

    left: usize,
    right: usize,
    buf: Vec<i32, Caps>,
    cap: usize,
    len: usize,
    pos: usize,
    frac: u32,

    nominal_step: u64,
    step_int: u32,
    step_frac: u32,
    adjust_ppb: Arc<AtomicI32, Caps>,
    applied_ppb: i32,
    latched: bool,

    taps: Vec<i32, Caps>,
    ntaps: usize,
    phase_bits: u32,
    up_l: u32,
    down_m: u32,
    phase: u32,
}

/// Safe from any task.
#[derive(Clone)]
pub struct ResamplerControl {
    adjust_ppb: Arc<AtomicI32, Caps>,
    kind: ResamplerKind,
}

impl ResamplerControl {
    /// Trims `in_rate/out_rate`; positive consumes input faster. Clamped to
    /// [`MAX_ADJUST_PPM`]. `Integer` returns `NotSupported`.
    pub fn set_adjust(&self, ppm: f32) -> Result<()> {
        if self.kind == ResamplerKind::Integer {
            return Err(Error::NotSupported);
        }
        let ppm = ppm.clamp(-MAX_ADJUST_PPM, MAX_ADJUST_PPM);
        self.adjust_ppb.store(lrintf(ppm * 1000.0), Ordering::SeqCst);
        Ok(())
    }
}

fn bessel_i0(x: f32) -> f32 {
    let mut sum = 1.0f32;
    let mut term = 1.0f32;
    let q = x * x * 0.25;
    for k in 1..32 {
        term *= q / (k * k) as f32;
        sum += term;
        if term < sum * 1e-9 {
            break;
        }
    }
    sum
}

/// `fc` is in cycles per input frame.
fn design_row(row: &mut [i32], tmp: &mut [f32], shift: f32, fc: f32) {
    let half = row.len() as f32 * 0.5;
    let inv_i0 = 1.0 / bessel_i0(KAISER_BETA);
    let mut sum = 0.0f32;
    for (j, v) in tmp.iter_mut().enumerate() {
        let t = j as f32 - (half - 1.0) - shift;
        let x = 2.0 * fc * t;
        let sinc = if x.abs() < 1e-6 { 1.0 } else { sinf(PI * x) / (PI * x) };
        let r = t / half;
        let w = if r * r < 1.0 { bessel_i0(KAISER_BETA * sqrtf(1.0 - r * r)) * inv_i0 } else { 0.0 };
        *v = sinc * w;
        sum += *v;
    }
    for (c, v) in row.iter_mut().zip(tmp.iter()) {
        *c = lrintf(v / sum * (1u32 << COEFF_Q) as f32);
    }
}

fn filled(len: usize, caps: Caps) -> Result<Vec<i32, Caps>> {
    let mut v = heap::try_vec(len, caps)?;
    v.resize(len, 0);
    Ok(v)
}

impl<S: Sample> Resampler<S> {
    pub fn new(config: &ResamplerConfig) -> Result<(Self, ResamplerControl)> {
        let core = Core::new(config)?;
        let control = core.control();
        Ok((Self { core, _sample: PhantomData }, control))
    }

    pub fn channels(&self) -> u8 {
        self.core.channels as u8
    }

    pub fn control(&self) -> ResamplerControl {
        self.core.control()
    }

    pub fn max_in_frames(&self) -> usize {
        self.core.max_in
    }

    pub fn reset(&mut self) {
        self.core.reset();
    }

    /// Input frames that make the next process yield exactly `out_frames`. The
    /// ratio is latched until that process.
    pub fn frames_needed(&mut self, out_frames: usize) -> usize {
        self.core.frames_needed(out_frames)
    }

    /// A buffer-sizing bound, independent of state.
    pub fn max_out(&self, in_frames: usize) -> usize {
        self.core.max_out(in_frames)
    }

    /// A buffer-sizing bound, independent of state.
    pub fn max_in(&self, out_frames: usize) -> usize {
        self.core.max_in(out_frames)
    }

    /// Takes all of `input` (at most `max_in_frames`) and returns the frames
    /// written. `InvalidSize` when `output` was too small to drain what
    /// `input` left behind.
    // Monomorphized here, so the loop builds at this crate's level and not at
    // the caller's.
    pub fn process(&mut self, input: &[S], output: &mut [S]) -> Result<usize> {
        let (ip, il) = (input.as_ptr(), input.len());
        let (op, ol) = (output.as_mut_ptr(), output.len());
        unsafe {
            match S::FORMAT {
                Format::S16 => self.core.process_s16(
                    slice::from_raw_parts(ip.cast(), il),
                    slice::from_raw_parts_mut(op.cast(), ol),
                ),
                Format::S32 => self.core.process_s32(
                    slice::from_raw_parts(ip.cast(), il),
                    slice::from_raw_parts_mut(op.cast(), ol),
                ),
            }
        }
    }
}

impl Core {
    fn new(config: &ResamplerConfig) -> Result<Self> {
        if config.in_rate == 0 || config.out_rate == 0 {
            return Err(Error::InvalidArg);
        }
        check_channels(config.channels)?;
        let caps = config.caps;
        let adjust_ppb = Arc::try_new_in(AtomicI32::new(0), caps).map_err(|_| Error::NoMemory)?;
        let mut r = Self {
            kind: config.kind,
            channels: usize::from(config.channels),
            in_rate: config.in_rate,
            out_rate: config.out_rate,
            max_in: if config.max_in_frames == 0 { DEFAULT_MAX_IN } else { config.max_in_frames },
            left: 0,
            right: 0,
            buf: Vec::new_in(caps),
            cap: 0,
            len: 0,
            pos: 0,
            frac: 0,
            nominal_step: (u64::from(config.in_rate) << 32) / u64::from(config.out_rate),
            step_int: 0,
            step_frac: 0,
            adjust_ppb,
            applied_ppb: 0,
            latched: false,
            taps: Vec::new_in(caps),
            ntaps: 0,
            phase_bits: 0,
            up_l: 0,
            down_m: 0,
            phase: 0,
        };
        r.update_step();

        let taps = if config.taps == 0 { DEFAULT_TAPS } else { usize::from(config.taps) };
        let taps = (taps + 1) & !1;
        match config.kind {
            ResamplerKind::Cubic => {
                r.left = 1;
                r.right = 2;
            }
            ResamplerKind::Polyphase => {
                let phases = if config.phases == 0 { DEFAULT_PHASES } else { u32::from(config.phases) };
                if phases > MAX_PHASES || !phases.is_power_of_two() {
                    return Err(Error::InvalidArg);
                }
                let ratio = config.out_rate as f32 / config.in_rate as f32;
                let scale = if ratio < 1.0 { ceilf(1.0 / ratio) as usize } else { 1 };
                let fc = 0.5 * PASSBAND * ratio.min(1.0);
                r.ntaps = taps * scale;
                r.phase_bits = phases.trailing_zeros();
                r.taps = filled((phases as usize + 1) * r.ntaps, caps)?;
                let mut tmp = heap::try_vec(r.ntaps, caps)?;
                tmp.resize(r.ntaps, 0.0f32);
                for (k, row) in r.taps.chunks_exact_mut(r.ntaps).enumerate() {
                    design_row(row, &mut tmp, k as f32 / phases as f32, fc);
                }
                r.left = r.ntaps / 2 - 1;
                r.right = r.ntaps / 2;
            }
            ResamplerKind::Integer => {
                let (mut l, mut m) = (1, 1);
                if config.out_rate % config.in_rate == 0 {
                    l = config.out_rate / config.in_rate;
                } else if config.in_rate % config.out_rate == 0 {
                    m = config.in_rate / config.out_rate;
                } else {
                    return Err(Error::InvalidArg);
                }
                r.up_l = l;
                r.down_m = m;
                r.ntaps = taps * m as usize;
                r.taps = filled(l as usize * r.ntaps, caps)?;
                let mut tmp = heap::try_vec(r.ntaps, caps)?;
                tmp.resize(r.ntaps, 0.0f32);
                for (k, row) in r.taps.chunks_exact_mut(r.ntaps).enumerate() {
                    design_row(row, &mut tmp, k as f32 / l as f32, 0.5 * PASSBAND / m as f32);
                }
                r.left = r.ntaps / 2 - 1;
                r.right = r.ntaps / 2;
            }
        }
        r.cap = r.left + r.right + r.max_in + r.step_int as usize + 2;
        r.buf = filled(r.cap * r.channels, caps)?;
        r.reset();
        Ok(r)
    }

    fn control(&self) -> ResamplerControl {
        ResamplerControl { adjust_ppb: self.adjust_ppb.clone(), kind: self.kind }
    }

    fn reset(&mut self) {
        self.buf[..self.left * self.channels].fill(0);
        self.len = self.left;
        self.pos = self.left;
        self.frac = 0;
        self.phase = 0;
        self.latched = false;
    }

    fn update_step(&mut self) {
        let ppb = self.adjust_ppb.load(Ordering::SeqCst);
        let mut step = self.nominal_step;
        if ppb != 0 {
            let delta = (step as f32 * (ppb as f32 * 1e-9)) as i64;
            step = (step as i64 + delta) as u64;
        }
        self.applied_ppb = ppb;
        self.step_int = (step >> 32) as u32;
        self.step_frac = step as u32;
    }

    fn refresh_step(&mut self) {
        if !self.latched && self.adjust_ppb.load(Ordering::SeqCst) != self.applied_ppb {
            self.update_step();
        }
    }

    fn last_index(&self, out_frames: usize) -> usize {
        let k = out_frames - 1;
        if self.kind == ResamplerKind::Integer {
            return self.pos + ((self.phase as usize + k * self.down_m as usize) / self.up_l as usize);
        }
        let step = u64::from(self.step_int) << 32 | u64::from(self.step_frac);
        self.pos + ((u64::from(self.frac) + k as u64 * step) >> 32) as usize
    }

    fn frames_needed(&mut self, out_frames: usize) -> usize {
        if out_frames == 0 {
            return 0;
        }
        self.latched = false;
        self.refresh_step();
        self.latched = true;
        (self.last_index(out_frames) + self.right + 1).saturating_sub(self.len)
    }

    fn max_out(&self, in_frames: usize) -> usize {
        let ratio = self.out_rate as f32 / self.in_rate as f32 * (1.0 + MAX_ADJUST_PPM * 1e-6);
        ceilf((in_frames + self.left + self.right + 2) as f32 * ratio) as usize + 2
    }

    fn max_in(&self, out_frames: usize) -> usize {
        let ratio = self.in_rate as f32 / self.out_rate as f32 * (1.0 + MAX_ADJUST_PPM * 1e-6);
        ceilf(out_frames as f32 * ratio) as usize + self.right + 2
    }

    #[inline(always)]
    fn generate<S: Sample>(&mut self, out: &mut [S], mut produced: usize, limit: usize) -> usize {
        let channels = self.channels;
        while produced < limit && self.pos + self.right < self.len {
            let start = (self.pos - self.left) * channels;
            for ch in 0..channels {
                let x = &self.buf[start + ch..];
                let y = match self.kind {
                    ResamplerKind::Cubic => cubic(x, channels, self.frac),
                    ResamplerKind::Polyphase => {
                        let k = if self.phase_bits != 0 { (self.frac >> (32 - self.phase_bits)) as usize } else { 0 };
                        let mu = i64::from((self.frac << self.phase_bits) >> (32 - MU_BITS));
                        let h0 = &self.taps[k * self.ntaps..(k + 1) * self.ntaps];
                        let h1 = &self.taps[(k + 1) * self.ntaps..(k + 2) * self.ntaps];
                        let y0 = (dot(x, channels, h0) + COEFF_HALF) >> COEFF_Q;
                        let y1 = (dot(x, channels, h1) + COEFF_HALF) >> COEFF_Q;
                        y0 + (((y1 - y0) * mu) >> MU_BITS)
                    }
                    ResamplerKind::Integer => {
                        let k = self.phase as usize;
                        let h = &self.taps[k * self.ntaps..(k + 1) * self.ntaps];
                        (dot(x, channels, h) + COEFF_HALF) >> COEFF_Q
                    }
                };
                out[produced * channels + ch] = S::store(y);
            }
            produced += 1;
            if self.kind == ResamplerKind::Integer {
                self.phase += self.down_m;
                self.pos += (self.phase / self.up_l) as usize;
                self.phase %= self.up_l;
            } else {
                let (frac, carry) = self.frac.overflowing_add(self.step_frac);
                self.pos += self.step_int as usize + usize::from(carry);
                self.frac = frac;
            }
        }
        produced
    }

    fn compact(&mut self) {
        let drop = (self.pos - self.left).min(self.len);
        if drop == 0 {
            return;
        }
        let channels = self.channels;
        self.buf.copy_within(drop * channels..self.len * channels, 0);
        self.len -= drop;
        self.pos -= drop;
    }

    #[inline(never)]
    fn process_s16(&mut self, input: &[i16], output: &mut [i16]) -> Result<usize> {
        self.process(input, output)
    }

    #[inline(never)]
    fn process_s32(&mut self, input: &[i32], output: &mut [i32]) -> Result<usize> {
        self.process(input, output)
    }

    #[inline(always)]
    fn process<S: Sample>(&mut self, input: &[S], output: &mut [S]) -> Result<usize> {
        let channels = self.channels;
        let in_frames = input.len() / channels;
        if in_frames > self.max_in {
            return Err(Error::InvalidSize);
        }
        self.refresh_step();
        self.latched = false;
        let limit = output.len() / channels;
        let (mut produced, mut consumed) = (0, 0);
        loop {
            let n = (in_frames - consumed).min(self.cap - self.len);
            let src = &input[consumed * channels..(consumed + n) * channels];
            let dst = &mut self.buf[self.len * channels..(self.len + n) * channels];
            for (d, s) in dst.iter_mut().zip(src) {
                *d = s.load();
            }
            self.len += n;
            consumed += n;
            produced = self.generate(output, produced, limit);
            self.compact();
            if consumed == in_frames {
                return Ok(produced);
            }
            if self.len == self.cap {
                return Err(Error::InvalidSize);
            }
        }
    }
}

#[inline]
fn dot(x: &[i32], stride: usize, h: &[i32]) -> i64 {
    x.iter().step_by(stride).zip(h).map(|(&x, &h)| i64::from(x) * i64::from(h)).sum()
}

#[inline]
fn cubic(x: &[i32], stride: usize, frac: u32) -> i64 {
    let xm1 = i64::from(x[0]);
    let x0 = i64::from(x[stride]);
    let x1 = i64::from(x[2 * stride]);
    let x2 = i64::from(x[3 * stride]);
    let t = i64::from(frac >> 16);
    let a = -xm1 + 3 * x0 - 3 * x1 + x2;
    let b = 2 * xm1 - 5 * x0 + 4 * x1 - x2;
    let c = x1 - xm1;
    let mut v = a;
    v = ((v * t) >> 16) + b;
    v = ((v * t) >> 16) + c;
    v = ((v * t) >> 16) + 2 * x0;
    v >> 1
}
