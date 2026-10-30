/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::f32::consts::PI;
use core::marker::PhantomData;

use devkit::Zeroable;
use devkit::math::{cosf, lrintf, powf, sinf, sqrtf};
use idf::heap::{self, Caps};

use crate::sample::check_channels;
use crate::sync::Shared;
use crate::{Error, Format, Result, Sample, sample::sat32};

const DEFAULT_MAX_STAGES: usize = 8;
const COEFF_Q: u32 = 28;
const COEFF_HALF: i64 = 1 << (COEFF_Q - 1);

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

#[derive(Clone, Copy, Default)]
struct BiquadQ {
    b0: i32,
    b1: i32,
    b2: i32,
    a1: i32,
    a2: i32,
}

#[derive(Clone, Copy, Default)]
struct State {
    z1: i64,
    z2: i64,
}

unsafe impl Zeroable for BiquadQ {}
unsafe impl Zeroable for State {}

fn coeff_to_q(c: f32) -> i32 {
    let v = c * (1u32 << COEFF_Q) as f32;
    if v >= 2147483647.0 {
        i32::MAX
    } else if v <= -2147483648.0 {
        i32::MIN
    } else {
        lrintf(v)
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
    unsafe { run(stages, states, channels, input, output, samples) }
}

#[inline(never)]
unsafe fn run_s32(stages: &[BiquadQ], states: *mut State, channels: usize, input: *const i32, output: *mut i32, samples: usize) {
    unsafe { run(stages, states, channels, input, output, samples) }
}

#[inline(always)]
unsafe fn run<S: Sample>(stages: &[BiquadQ], states: *mut State, channels: usize, input: *const S, output: *mut S, samples: usize) {
    for f in 0..samples / channels {
        for ch in 0..channels {
            let i = f * channels + ch;
            unsafe {
                let mut x = (*input.add(i)).load();
                for (s, b) in stages.iter().enumerate() {
                    let z = &mut *states.add(s * channels + ch);
                    let acc = i64::from(b.b0) * i64::from(x) + z.z1;
                    let y = sat32((acc + COEFF_HALF) >> COEFF_Q);
                    z.z1 = i64::from(b.b1) * i64::from(x) - i64::from(b.a1) * i64::from(y) + z.z2;
                    z.z2 = i64::from(b.b2) * i64::from(x) - i64::from(b.a2) * i64::from(y);
                    x = y;
                }
                *output.add(i) = S::store(i64::from(x));
            }
        }
    }
}

impl EqControl {
    pub fn set_biquads(&self, biquads: &[Biquad]) -> Result<()> {
        self.shared.publish(|p| {
            if biquads.len() > p.biquads.len() {
                return Err(Error::InvalidSize);
            }
            for (q, b) in p.biquads.iter_mut().zip(biquads) {
                *q = BiquadQ {
                    b0: coeff_to_q(b.b0),
                    b1: coeff_to_q(b.b1),
                    b2: coeff_to_q(b.b2),
                    a1: coeff_to_q(b.a1),
                    a2: coeff_to_q(b.a2),
                };
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
