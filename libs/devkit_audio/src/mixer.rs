/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::marker::PhantomData;

use devkit::math::lrintf;
use idf::heap::{self, Caps};

use crate::sample::{Format, check_channels, mulh, mulh_scale, shift_sat};
use crate::sync::Shared;
use crate::{Error, MAX_CHANNELS, Result, Sample};

const MIX_Q: u32 = 24;
const MIX_UNITY: i32 = 1 << MIX_Q;
const MIX_MAX: f32 = 127.0;

#[derive(Clone, Copy, Debug)]
pub struct MixerConfig<'a> {
    pub out_channels: u8,
    pub in_channels: &'a [u8],
    pub caps: Caps,
}

struct Input {
    channels: usize,
    active: Vec<i32, Caps>,
    scaled: Vec<i32, Caps>,
    identity: bool,
}

type Matrices = Vec<Vec<i32, Caps>, Caps>;

/// Mixes inputs through one matrix each. Every input starts with a
/// pass-through matrix: identity, 1->N copies, N->1 averages.
pub struct Mixer<S: Sample> {
    shared: Arc<Shared<Matrices>, Caps>,
    out_channels: usize,
    inputs: Vec<Input, Caps>,
    sources: Vec<usize, Caps>,
    shift: i32,
    _sample: PhantomData<S>,
}

/// Safe from any task; takes effect at the next process.
#[derive(Clone)]
pub struct MixerControl {
    shared: Arc<Shared<Matrices>, Caps>,
    out_channels: usize,
}

fn coeff_to_q(c: f32) -> i32 {
    lrintf(c.clamp(-MIX_MAX, MIX_MAX) * MIX_UNITY as f32)
}

fn default_matrix(m: &mut [i32], out_ch: usize, in_ch: usize) {
    m.fill(0);
    for o in 0..out_ch {
        if in_ch == 1 {
            m[o] = MIX_UNITY;
        } else if out_ch == 1 {
            m[..in_ch].fill(MIX_UNITY / in_ch as i32);
        } else if o < in_ch {
            m[o * in_ch + o] = MIX_UNITY;
        }
    }
}

fn is_identity(m: &[i32], out_ch: usize, in_ch: usize) -> bool {
    out_ch == in_ch
        && (0..out_ch).all(|o| (0..in_ch).all(|i| m[o * in_ch + i] == if o == i { MIX_UNITY } else { 0 }))
}

impl<S: Sample> Mixer<S> {
    pub fn new(config: &MixerConfig<'_>) -> Result<(Self, MixerControl)> {
        if config.in_channels.is_empty() || config.in_channels.len() > 255 {
            return Err(Error::InvalidArg);
        }
        check_channels(config.out_channels)?;
        let caps = config.caps;
        let out_channels = usize::from(config.out_channels);
        let mut inputs = heap::try_vec(config.in_channels.len(), caps)?;
        let mut pending: Matrices = heap::try_vec(config.in_channels.len(), caps)?;
        for &ch in config.in_channels {
            check_channels(ch)?;
            let channels = usize::from(ch);
            let count = out_channels * channels;
            let mut m = heap::try_vec(count, caps)?;
            m.resize(count, 0);
            default_matrix(&mut m, out_channels, channels);
            let mut active = heap::try_vec(count, caps)?;
            active.extend_from_slice(&m);
            let mut scaled = heap::try_vec(count, caps)?;
            scaled.resize(count, 0);
            let identity = is_identity(&m, out_channels, channels);
            inputs.push(Input { channels, active, scaled, identity });
            pending.push(m);
        }
        let mut sources = heap::try_vec(inputs.len(), caps)?;
        sources.resize(inputs.len(), 0);
        let shared = Shared::new(pending, caps)?;
        let mut mixer = Self { shared: shared.clone(), out_channels, inputs, sources, shift: 0, _sample: PhantomData };
        mixer.rescale();
        Ok((mixer, MixerControl { shared, out_channels }))
    }

    pub fn num_inputs(&self) -> usize {
        self.inputs.len()
    }

    pub fn out_channels(&self) -> u8 {
        self.out_channels as u8
    }

    pub fn in_channels(&self, input: usize) -> u8 {
        self.inputs[input].channels as u8
    }

    fn adopt(&mut self) {
        let Some(pending) = self.shared.try_adopt() else { return };
        for (input, m) in self.inputs.iter_mut().zip(pending.iter()) {
            input.active.copy_from_slice(m);
            input.identity = is_identity(m, self.out_channels, input.channels);
        }
        drop(pending);
        self.rescale();
    }

    fn rescale(&mut self) {
        let max = self.inputs.iter().flat_map(|input| input.active.iter()).map(|c| c.unsigned_abs()).max().unwrap_or(0);
        let (n, shift) = mulh_scale(max as i32, MIX_Q);
        for input in &mut self.inputs {
            for (s, &c) in input.scaled.iter_mut().zip(&input.active) {
                *s = c << n;
            }
        }
        self.shift = shift;
    }

    fn frames(&self, inputs: &[Option<&[S]>], output: &[S]) -> usize {
        let frames = output.len() / self.out_channels;
        assert_eq!(inputs.len(), self.inputs.len());
        for (input, mi) in inputs.iter().zip(&self.inputs) {
            if let Some(input) = input {
                assert!(input.len() >= frames * mi.channels);
            }
        }
        frames
    }

    /// `None` mixes as silence.
    pub fn process(&mut self, inputs: &[Option<&[S]>], output: &mut [S]) {
        let frames = self.frames(inputs, output);
        self.adopt();
        if self.inputs.len() == 1 && self.inputs[0].identity {
            if let Some(input) = inputs[0] {
                output.copy_from_slice(&input[..output.len()]);
                return;
            }
        }
        unsafe { self.run(|n| inputs[n].map(<[S]>::as_ptr), output.as_mut_ptr(), frames) };
    }

    pub(crate) fn process_with<'x>(&mut self, input: impl Fn(usize) -> Option<&'x [S]>, output: &mut [S]) {
        self.adopt();
        let frames = output.len() / self.out_channels;
        if self.inputs.len() == 1 && self.inputs[0].identity {
            if let Some(input) = input(0) {
                output.copy_from_slice(&input[..output.len()]);
                return;
            }
        }
        unsafe { self.run(|n| input(n).map(<[S]>::as_ptr), output.as_mut_ptr(), frames) };
    }

    /// `pcm` is input 0, which needs at least `out_channels` channels, and the
    /// output; `rest` are the other inputs.
    pub fn process_in_place(&mut self, pcm: &mut [S], rest: &[Option<&[S]>]) {
        assert_eq!(rest.len() + 1, self.inputs.len());
        let in_channels = self.inputs[0].channels;
        assert!(in_channels >= self.out_channels);
        let frames = pcm.len() / in_channels;
        for (input, mi) in rest.iter().zip(&self.inputs[1..]) {
            if let Some(input) = input {
                assert!(input.len() >= frames * mi.channels);
            }
        }
        self.adopt();
        if self.inputs.len() == 1 && self.inputs[0].identity {
            return;
        }
        let p = pcm.as_mut_ptr();
        let input = |n: usize| if n == 0 { Some(p.cast_const()) } else { rest[n - 1].map(<[S]>::as_ptr) };
        unsafe { self.run(input, p, frames) };
    }

    // Monomorphized here, so the loop builds at this crate's level and not at
    // the caller's.
    unsafe fn run(&mut self, input: impl Fn(usize) -> Option<*const S>, output: *mut S, frames: usize) {
        for (n, s) in self.sources.iter_mut().enumerate() {
            *s = input(n).map_or(0, |p| p as usize);
        }
        match S::FORMAT {
            Format::S16 => unsafe { run_s16(&self.inputs, &self.sources, self.out_channels, self.shift, output.cast(), frames) },
            Format::S32 => unsafe { run_s32(&self.inputs, &self.sources, self.out_channels, self.shift, output.cast(), frames) },
        }
    }
}

#[inline(never)]
unsafe fn run_s16(inputs: &[Input], sources: &[usize], out_ch: usize, shift: i32, output: *mut i16, frames: usize) {
    unsafe { mix(inputs, sources, out_ch, shift, output, frames) }
}

#[inline(never)]
unsafe fn run_s32(inputs: &[Input], sources: &[usize], out_ch: usize, shift: i32, output: *mut i32, frames: usize) {
    unsafe { mix(inputs, sources, out_ch, shift, output, frames) }
}

// Sums wrap rather than saturate before the shift; a mix only gets there far
// beyond the S32 headroom.
#[inline(always)]
unsafe fn mix<S: Sample>(inputs: &[Input], sources: &[usize], out_ch: usize, shift: i32, output: *mut S, frames: usize) {
    if let ([mi], [source]) = (inputs, sources)
        && mi.channels == 2
        && out_ch == 2
        && *source != 0
    {
        let p = *source as *const S;
        let [m00, m01, m10, m11] = [mi.scaled[0], mi.scaled[1], mi.scaled[2], mi.scaled[3]];
        for f in 0..frames {
            let (l, r) = unsafe { ((*p.add(2 * f)).load(), (*p.add(2 * f + 1)).load()) };
            let a = mulh(l, m00).wrapping_add(mulh(r, m01));
            let b = mulh(l, m10).wrapping_add(mulh(r, m11));
            unsafe {
                *output.add(2 * f) = S::store32(shift_sat(a, shift));
                *output.add(2 * f + 1) = S::store32(shift_sat(b, shift));
            }
        }
        return;
    }
    for f in 0..frames {
        let mut acc = [0i32; MAX_CHANNELS as usize];
        for (mi, &source) in inputs.iter().zip(sources) {
            if source == 0 {
                continue;
            }
            let in_ch = mi.channels;
            let p = unsafe { (source as *const S).add(f * in_ch) };
            let m = mi.scaled.as_ptr();
            for (o, a) in acc[..out_ch].iter_mut().enumerate() {
                for i in 0..in_ch {
                    *a = a.wrapping_add(mulh(unsafe { *p.add(i) }.load(), unsafe { *m.add(o * in_ch + i) }));
                }
            }
        }
        for (o, a) in acc[..out_ch].iter().enumerate() {
            unsafe { *output.add(f * out_ch + o) = S::store32(shift_sat(*a, shift)) };
        }
    }
}

impl MixerControl {
    /// `matrix` is `out_channels x in_channels`, row-major.
    pub fn set_matrix(&self, input: usize, matrix: &[f32]) -> Result<()> {
        self.shared.publish(|m| {
            let pending = m.get_mut(input).ok_or(Error::InvalidArg)?;
            if matrix.len() != pending.len() {
                return Err(Error::InvalidSize);
            }
            for (q, &c) in pending.iter_mut().zip(matrix) {
                *q = coeff_to_q(c);
            }
            Ok(())
        })
    }

    pub fn matrix(&self, input: usize, matrix: &mut [f32]) -> Result<()> {
        self.shared.read(|m| {
            let pending = m.get(input).ok_or(Error::InvalidArg)?;
            if matrix.len() != pending.len() {
                return Err(Error::InvalidSize);
            }
            for (c, &q) in matrix.iter_mut().zip(pending) {
                *c = q as f32 / MIX_UNITY as f32;
            }
            Ok(())
        })
    }

    pub fn out_channels(&self) -> u8 {
        self.out_channels as u8
    }
}
