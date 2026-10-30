/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use core::marker::PhantomData;

use devkit::math::lrintf;
use idf::heap::Caps;

use crate::sample::check_channels;
use crate::sync::Shared;
use crate::{Error, Format, Result, Sample};

pub const GAIN_MAX: f32 = 127.0;

const GAIN_Q: u32 = 24;
const GAIN_UNITY: i32 = 1 << GAIN_Q;
const GAIN_HALF: i64 = 1 << (GAIN_Q - 1);

#[derive(Clone, Copy, Debug)]
pub struct GainConfig {
    pub channels: u8,
    pub sample_rate: u32,
    /// Linear, initial.
    pub gain: f32,
    pub caps: Caps,
}

impl Default for GainConfig {
    fn default() -> Self {
        Self { channels: 2, sample_rate: 48000, gain: 1.0, caps: Caps::DEFAULT }
    }
}

struct Pending {
    target: f32,
    target_q: i32,
    fade_ms: u32,
    jump: Option<i32>,
}

/// Linear gain with a frame-by-frame fade. Changes go through [`GainControl`].
pub struct Gain<S: Sample> {
    shared: Arc<Shared<Pending>, Caps>,
    channels: usize,
    sample_rate: u32,
    current_q: i32,
    target_q: i32,
    step_q: i32,
    fade_remaining: u32,
    _sample: PhantomData<S>,
}

/// Safe from any task; takes effect at the next process.
#[derive(Clone)]
pub struct GainControl {
    shared: Arc<Shared<Pending>, Caps>,
}

fn clamp_gain(g: f32) -> f32 {
    if g > 0.0 { g.min(GAIN_MAX) } else { 0.0 }
}

fn gain_to_q(g: f32) -> i32 {
    lrintf(g * GAIN_UNITY as f32)
}

impl<S: Sample> Gain<S> {
    pub fn new(config: &GainConfig) -> Result<(Self, GainControl)> {
        if config.sample_rate == 0 {
            return Err(Error::InvalidArg);
        }
        check_channels(config.channels)?;
        let target = clamp_gain(config.gain);
        let q = gain_to_q(target);
        let shared = Shared::new(Pending { target, target_q: q, fade_ms: 0, jump: None }, config.caps)?;
        let gain = Self {
            shared: shared.clone(),
            channels: usize::from(config.channels),
            sample_rate: config.sample_rate,
            current_q: q,
            target_q: q,
            step_q: 0,
            fade_remaining: 0,
            _sample: PhantomData,
        };
        Ok((gain, GainControl { shared }))
    }

    pub fn channels(&self) -> u8 {
        self.channels as u8
    }

    /// Keeps the gain and any fade.
    pub fn reconfig(&mut self, channels: u8, sample_rate: u32) -> Result<()> {
        if sample_rate == 0 {
            return Err(Error::InvalidArg);
        }
        check_channels(channels)?;
        self.channels = usize::from(channels);
        self.sample_rate = sample_rate;
        Ok(())
    }

    fn adopt(&mut self) {
        let Some(mut pending) = self.shared.try_adopt() else { return };
        let target_q = pending.target_q;
        let fade_ms = pending.fade_ms;
        if let Some(jump) = pending.jump.take() {
            self.current_q = jump;
        }
        drop(pending);

        self.target_q = target_q;
        let frames = ((u64::from(self.sample_rate) * u64::from(fade_ms) + 500) / 1000) as u32;
        if frames == 0 || self.current_q == target_q {
            self.current_q = target_q;
            self.fade_remaining = 0;
            self.step_q = 0;
        } else {
            self.fade_remaining = frames;
            self.step_q = (target_q - self.current_q) / frames as i32;
        }
    }

    pub fn process(&mut self, input: &[S], output: &mut [S]) {
        assert_eq!(input.len(), output.len());
        self.adopt();
        if self.current_q == GAIN_UNITY && self.fade_remaining == 0 {
            output.copy_from_slice(input);
            return;
        }
        unsafe { self.run(input.as_ptr(), output.as_mut_ptr(), input.len()) };
    }

    pub fn process_in_place(&mut self, pcm: &mut [S]) {
        self.adopt();
        if self.current_q == GAIN_UNITY && self.fade_remaining == 0 {
            return;
        }
        let p = pcm.as_mut_ptr();
        unsafe { self.run(p, p, pcm.len()) };
    }

    // Monomorphized here, so the loop builds at this crate's level and not at
    // the caller's.
    unsafe fn run(&mut self, input: *const S, output: *mut S, samples: usize) {
        let fade = Fade { channels: self.channels, g: self.current_q, remaining: self.fade_remaining, step: self.step_q, target: self.target_q };
        let (g, remaining) = unsafe {
            match S::FORMAT {
                Format::S16 => run_s16(fade, input.cast(), output.cast(), samples),
                Format::S32 => run_s32(fade, input.cast(), output.cast(), samples),
            }
        };
        self.current_q = g;
        self.fade_remaining = remaining;
    }
}

#[derive(Clone, Copy)]
struct Fade {
    channels: usize,
    g: i32,
    remaining: u32,
    step: i32,
    target: i32,
}

#[inline(never)]
unsafe fn run_s16(fade: Fade, input: *const i16, output: *mut i16, samples: usize) -> (i32, u32) {
    unsafe { run(fade, input, output, samples) }
}

#[inline(never)]
unsafe fn run_s32(fade: Fade, input: *const i32, output: *mut i32, samples: usize) -> (i32, u32) {
    unsafe { run(fade, input, output, samples) }
}

#[inline(always)]
unsafe fn run<S: Sample>(fade: Fade, input: *const S, output: *mut S, samples: usize) -> (i32, u32) {
    let channels = fade.channels;
    let mut g = fade.g;
    let mut remaining = fade.remaining;
    for f in 0..samples / channels {
        if remaining != 0 {
            g += fade.step;
            remaining -= 1;
            if remaining == 0 {
                g = fade.target;
            }
        }
        for ch in 0..channels {
            let i = f * channels + ch;
            let x = i64::from(unsafe { *input.add(i) }.load());
            unsafe { *output.add(i) = S::store((x * i64::from(g) + GAIN_HALF) >> GAIN_Q) };
        }
    }
    (g, remaining)
}

impl GainControl {
    /// Linear, clamped to `0..=GAIN_MAX`. `fade_ms = 0` jumps, and a fade
    /// requested before the next process starts from that jump.
    pub fn set(&self, target: f32, fade_ms: u32) {
        let target = clamp_gain(target);
        self.shared.publish(|p| {
            p.target = target;
            p.target_q = gain_to_q(target);
            p.fade_ms = fade_ms;
            if fade_ms == 0 {
                p.jump = Some(p.target_q);
            }
        });
    }

    pub fn get(&self) -> f32 {
        self.shared.read(|p| p.target)
    }
}
