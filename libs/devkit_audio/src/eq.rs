/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::f32::consts::PI;
use core::marker::PhantomData;

use devkit::Zeroable;
use devkit::math::{cosf, powf, sinf, sqrtf};
use idf::heap::{self, Caps};

use crate::sample::check_channels;
use crate::sync::Shared;
use crate::sample::mulh_round;
use crate::{Error, Format, Result, Sample};

const DEFAULT_MAX_STAGES: usize = 8;
/// Working values run this many bits up inside the chain: a stage wraps past
/// 24 dB over the S32 full scale. Inputs clamp at 6 dB over it.
const SHIFT: u32 = 4;
const IN_LIMIT: i32 = 1 << 24;
const MAX_FRAC: i32 = 24;

/// Normalised to `a0 = 1`.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Biquad {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

fn normalize(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Biquad {
    let inv = 1.0 / a0;
    Biquad { b0: b0 * inv, b1: b1 * inv, b2: b2 * inv, a1: a1 * inv, a2: a2 * inv }
}

impl Biquad {
    pub fn peaking(fs: u32, f0: f32, q: f32, gain_db: f32) -> Self {
        let a = powf(10.0, gain_db / 40.0);
        let w0 = 2.0 * PI * f0 / fs as f32;
        let cos_w0 = cosf(w0);
        let alpha = sinf(w0) / (2.0 * q);
        normalize(1.0 + alpha * a, -2.0 * cos_w0, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos_w0, 1.0 - alpha / a)
    }

    pub fn low_shelf(fs: u32, f0: f32, q: f32, gain_db: f32) -> Self {
        let a = powf(10.0, gain_db / 40.0);
        let w0 = 2.0 * PI * f0 / fs as f32;
        let cos_w0 = cosf(w0);
        let alpha = sinf(w0) / (2.0 * q);
        let beta = 2.0 * sqrtf(a) * alpha;
        normalize(
            a * ((a + 1.0) - (a - 1.0) * cos_w0 + beta),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0),
            a * ((a + 1.0) - (a - 1.0) * cos_w0 - beta),
            (a + 1.0) + (a - 1.0) * cos_w0 + beta,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0),
            (a + 1.0) + (a - 1.0) * cos_w0 - beta,
        )
    }

    pub fn high_shelf(fs: u32, f0: f32, q: f32, gain_db: f32) -> Self {
        let a = powf(10.0, gain_db / 40.0);
        let w0 = 2.0 * PI * f0 / fs as f32;
        let cos_w0 = cosf(w0);
        let alpha = sinf(w0) / (2.0 * q);
        let beta = 2.0 * sqrtf(a) * alpha;
        normalize(
            a * ((a + 1.0) + (a - 1.0) * cos_w0 + beta),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0),
            a * ((a + 1.0) + (a - 1.0) * cos_w0 - beta),
            (a + 1.0) - (a - 1.0) * cos_w0 + beta,
            2.0 * ((a - 1.0) - (a + 1.0) * cos_w0),
            (a + 1.0) - (a - 1.0) * cos_w0 - beta,
        )
    }

    pub fn lowpass(fs: u32, f0: f32, q: f32) -> Self {
        let w0 = 2.0 * PI * f0 / fs as f32;
        let cos_w0 = cosf(w0);
        let alpha = sinf(w0) / (2.0 * q);
        let k = (1.0 - cos_w0) * 0.5;
        normalize(k, 2.0 * k, k, 1.0 + alpha, -2.0 * cos_w0, 1.0 - alpha)
    }

    pub fn highpass(fs: u32, f0: f32, q: f32) -> Self {
        let w0 = 2.0 * PI * f0 / fs as f32;
        let cos_w0 = cosf(w0);
        let alpha = sinf(w0) / (2.0 * q);
        let k = (1.0 + cos_w0) * 0.5;
        normalize(k, -2.0 * k, k, 1.0 + alpha, -2.0 * cos_w0, 1.0 - alpha)
    }
}

// Direct form I in 32 bits as y = 2·y1 - y2 + b0·(x - 2·x1 + x2) + c1·x1 + c2·x2
// + d1·y1 + d2·y2 rather than with a1 and a2: for a low-frequency stage the large
// parts stay exact and the small terms keep `frac` bits to the next sample;
// rounded straight, it drifts and sticks tens of dB above the S16 LSB.
#[derive(Clone, Copy, Default)]
struct BiquadQ {
    b0: i32,
    c1: i32,
    c2: i32,
    d1: i32,
    d2: i32,
    coarse: u32,
    fine: u32,
    frac: u32,
}

#[derive(Clone, Copy, Default)]
struct State {
    x1: i32,
    x2: i32,
    y1: i32,
    y2: i32,
    r: i32,
}

unsafe impl Zeroable for BiquadQ {}
unsafe impl Zeroable for State {}

/// The smallest `e` with `v < 2^e`.
fn exponent(v: f64) -> i32 {
    let mut e = 0;
    let mut p = 1.0;
    while v >= p {
        p *= 2.0;
        e += 1;
    }
    while e > -64 && v < p * 0.5 {
        p *= 0.5;
        e -= 1;
    }
    e
}

fn pow2(e: i32) -> f64 {
    let mut p = 1.0;
    for _ in 0..e.unsigned_abs() {
        p = if e > 0 { p * 2.0 } else { p * 0.5 };
    }
    p
}

fn to_q(c: f64, e: i32) -> i32 {
    let v = c * pow2(e);
    let v = if v >= 0.0 { v + 0.5 } else { v - 0.5 };
    (v as i64).clamp(-(i32::MAX as i64), i32::MAX as i64) as i32
}

impl BiquadQ {
    fn new(b: &Biquad) -> Self {
        let (b0, b1, b2, a1, a2) = (f64::from(b.b0), f64::from(b.b1), f64::from(b.b2), f64::from(b.a1), f64::from(b.a2));
        let fine = [b1 + 2.0 * b0, b2 - b0, -(a1 + 2.0), 1.0 - a2];
        let e = exponent(b0.abs()).max(-1);
        let max = fine.iter().fold(0.0f64, |m, c| m.max(c.abs()));
        let (fine_shift, frac, scale) = if max == 0.0 {
            (0, 0, 0)
        } else {
            let e = exponent(max);
            if e >= -1 { (1 + e, 0, 31 - e) } else { (0, (-1 - e).min(MAX_FRAC), 32 + (-1 - e).min(MAX_FRAC)) }
        };
        Self {
            b0: to_q(b0, 31 - e),
            c1: to_q(fine[0], scale),
            c2: to_q(fine[1], scale),
            d1: to_q(fine[2], scale),
            d2: to_q(fine[3], scale),
            coarse: (1 + e) as u32,
            fine: fine_shift as u32,
            frac: frac as u32,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct EqConfig {
    pub channels: u8,
    /// 0 = 8.
    pub max_stages: usize,
    pub enabled: bool,
    pub caps: Caps,
}

impl Default for EqConfig {
    fn default() -> Self {
        Self { channels: 2, max_stages: 0, enabled: false, caps: Caps::DEFAULT }
    }
}

struct Pending {
    biquads: Vec<BiquadQ, Caps>,
    stages: usize,
    enabled: bool,
}

/// Biquad chain. Coefficient changes go through [`EqControl`] and keep the
/// filter state, so a slider can move without clicks.
pub struct Eq<S: Sample> {
    shared: Arc<Shared<Pending>, Caps>,
    channels: usize,
    caps: Caps,
    active: Vec<BiquadQ, Caps>,
    active_stages: usize,
    active_enabled: bool,
    states: Vec<State, Caps>,
    _sample: PhantomData<S>,
}

/// Safe from any task; takes effect at the next process.
#[derive(Clone)]
pub struct EqControl {
    shared: Arc<Shared<Pending>, Caps>,
}

fn zeroed<T: Zeroable + Clone + Default>(len: usize, caps: Caps) -> Result<Vec<T, Caps>> {
    let mut v = heap::try_vec(len, caps)?;
    v.resize(len, T::default());
    Ok(v)
}

impl<S: Sample> Eq<S> {
    pub fn new(config: &EqConfig) -> Result<(Self, EqControl)> {
        check_channels(config.channels)?;
        let caps = config.caps;
        let max_stages = if config.max_stages == 0 { DEFAULT_MAX_STAGES } else { config.max_stages };
        let channels = usize::from(config.channels);
        let pending = Pending { biquads: zeroed(max_stages, caps)?, stages: 0, enabled: config.enabled };
        let shared = Shared::new(pending, caps)?;
        let eq = Self {
            shared: shared.clone(),
            channels,
            caps,
            active: zeroed(max_stages, caps)?,
            active_stages: 0,
            active_enabled: config.enabled,
            states: zeroed(max_stages * channels, caps)?,
            _sample: PhantomData,
        };
        Ok((eq, EqControl { shared }))
    }

    pub fn channels(&self) -> u8 {
        self.channels as u8
    }

    /// Resets the filter state.
    pub fn reconfig(&mut self, channels: u8) -> Result<()> {
        check_channels(channels)?;
        let channels = usize::from(channels);
        if channels != self.channels {
            self.states = zeroed(self.active.len() * channels, self.caps)?;
            self.channels = channels;
        } else {
            self.states.fill(State::default());
        }
        Ok(())
    }

    fn adopt(&mut self) {
        let Some(pending) = self.shared.try_adopt() else { return };
        let n = pending.stages;
        self.active[..n].copy_from_slice(&pending.biquads[..n]);
        if !self.active_enabled && pending.enabled {
            self.states.fill(State::default());
        } else if n > self.active_stages {
            self.states[self.active_stages * self.channels..n * self.channels].fill(State::default());
        }
        self.active_stages = n;
        self.active_enabled = pending.enabled;
    }

    pub fn process(&mut self, input: &[S], output: &mut [S]) {
        assert_eq!(input.len(), output.len());
        self.adopt();
        if !self.active_enabled || self.active_stages == 0 {
            output.copy_from_slice(input);
            return;
        }
        unsafe { self.run(input.as_ptr(), output.as_mut_ptr(), input.len()) };
    }

    pub fn process_in_place(&mut self, pcm: &mut [S]) {
        self.adopt();
        if !self.active_enabled || self.active_stages == 0 {
            return;
        }
        let p = pcm.as_mut_ptr();
        unsafe { self.run(p, p, pcm.len()) };
    }

    // Monomorphized here, so the loop builds at this crate's level and not at
    // the caller's.
    unsafe fn run(&mut self, input: *const S, output: *mut S, samples: usize) {
        let stages = &self.active[..self.active_stages];
        let states = self.states.as_mut_ptr();
        unsafe {
            match S::FORMAT {
                Format::S16 => run_s16(stages, states, self.channels, input.cast(), output.cast(), samples),
                Format::S32 => run_s32(stages, states, self.channels, input.cast(), output.cast(), samples),
            }
        }
    }
}

#[inline(never)]
unsafe fn run_s16(stages: &[BiquadQ], states: *mut State, channels: usize, input: *const i16, output: *mut i16, samples: usize) {
    for f in 0..samples / channels {
        for ch in 0..channels {
            let i = f * channels + ch;
            unsafe {
                let mut x = load(*input.add(i));
                for (s, b) in stages.iter().enumerate() {
                    x = step(b, &mut *states.add(s * channels + ch), x);
                }
                *output.add(i) = store(x);
            }
        }
    }
}

// Stage by stage through `output`, which holds the shifted values in between,
// so that each stage keeps its coefficients and state in registers.
#[inline(never)]
unsafe fn run_s32(stages: &[BiquadQ], states: *mut State, channels: usize, input: *const i32, output: *mut i32, samples: usize) {
    let frames = samples / channels;
    let last = stages.len() - 1;
    for ch in 0..channels {
        for (s, b) in stages.iter().enumerate() {
            let b = *b;
            let state = unsafe { &mut *states.add(s * channels + ch) };
            let mut z = *state;
            let src = if s == 0 { input } else { output.cast_const() };
            for f in 0..frames {
                let i = f * channels + ch;
                unsafe {
                    let x = *src.add(i);
                    let x = if s == 0 { load(x) } else { x };
                    let y = step(&b, &mut z, x);
                    *output.add(i) = if s == last { store(y) } else { y };
                }
            }
            *state = z;
        }
    }
}

#[inline(always)]
fn load<S: Sample>(x: S) -> i32 {
    x.load().clamp(-IN_LIMIT, IN_LIMIT) << SHIFT
}

#[inline(always)]
fn store<S: Sample>(y: i32) -> S {
    S::store32(((y >> (SHIFT - 1)) + 1) >> 1)
}

#[inline(always)]
fn step(b: &BiquadQ, z: &mut State, x: i32) -> i32 {
    let diff = x.wrapping_sub(z.x1.wrapping_mul(2)).wrapping_add(z.x2);
    let coarse = mulh_round(diff, b.b0) << b.coarse;
    let fine = mulh_round(z.x1, b.c1)
        .wrapping_add(mulh_round(z.x2, b.c2))
        .wrapping_add(mulh_round(z.y1, b.d1))
        .wrapping_add(mulh_round(z.y2, b.d2));
    let acc = (fine << b.fine).wrapping_add(z.r);
    let q = acc >> b.frac;
    z.r = acc.wrapping_sub(q << b.frac);
    let y = z.y1.wrapping_mul(2).wrapping_sub(z.y2).wrapping_add(coarse).wrapping_add(q);
    z.x2 = z.x1;
    z.x1 = x;
    z.y2 = z.y1;
    z.y1 = y;
    y
}

impl EqControl {
    pub fn set_biquads(&self, biquads: &[Biquad]) -> Result<()> {
        self.shared.publish(|p| {
            if biquads.len() > p.biquads.len() {
                return Err(Error::InvalidSize);
            }
            for (q, b) in p.biquads.iter_mut().zip(biquads) {
                *q = BiquadQ::new(b);
            }
            p.stages = biquads.len();
            Ok(())
        })
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.shared.publish(|p| p.enabled = enabled);
    }

    pub fn enabled(&self) -> bool {
        self.shared.read(|p| p.enabled)
    }
}
