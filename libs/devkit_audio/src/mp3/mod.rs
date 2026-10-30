/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

mod kernels;
mod l2;
mod l3;
mod tables;

use alloc::boxed::Box;

use devkit::{A16, Zeroable};
use idf::heap::{self, Caps};

use crate::{Decoder, Error, Format, PcmMut, Result};
use kernels::split;
use tables::{DCT16_SPLIT, DCT16E_SPLIT};

pub const MP3_HEADER_BYTES: usize = 4;
pub const MP3_MAX_FRAMES: usize = 1152;

// Spectral and subband samples are PCM x 2^FRAC.
const FRAC: u32 = 24;
const MAX_FRAME: usize = 1732;
const MAX_BACK: usize = 511;
const PAD: usize = 16;

const MODE_JOINT: u8 = 1;
const MODE_MONO: u8 = 3;

const SYNTH_FRAC: u32 = 23;
const SYNTH_IN_MAX: i32 = (1 << 26) - 1;
const SYNTH_V_MAX: i32 = 1 << 28;
const SYNTH_V_FRAC: u32 = SYNTH_FRAC + 1;
// Half of the 22 window rows, whose low products each round down.
const SYNTH_FLOOR_BIAS: i32 = 11;

const KBPS: [[u16; 15]; 3] = [
    [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384],
    [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
];

const RATES: [u32; 3] = [44100, 48000, 32000];

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Mp3Header {
    pub rate: u32,
    /// The whole frame, header included.
    pub bytes: u16,
    /// PCM frames it decodes to.
    pub frames: u16,
    pub channels: u8,
    pub layer: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct Mp3Config {
    /// Output channels, 1 or 2.
    pub channels: u8,
    pub caps: Caps,
}

#[derive(Clone, Copy, Default)]
struct Frame {
    h: Mp3Header,
    kbps: u16,
    version: u8,
    rate_index: u8,
    mode: u8,
    mode_ext: u8,
    crc: bool,
}

#[inline(always)]
fn mulh(a: i32, b: i32) -> i32 {
    ((i64::from(a) * i64::from(b)) >> 32) as i32
}

// Left shift that brings `mag` (an OR of magnitudes) just under 2^bits.
#[inline]
fn headroom_of(mag: u32, bits: i32) -> u32 {
    if mag == 0 { 0 } else { (bits - (32 - mag.leading_zeros() as i32)).max(0) as u32 }
}

#[inline]
fn headroom(x: &[i32], bits: i32) -> u32 {
    headroom_of(x.iter().fold(0, |m, v| m | v.unsigned_abs()), bits)
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Synth {
    vh: A16<[i16; 2048]>,
    vl: A16<[i16; 2048]>,
    off: usize,
}

#[repr(C)]
struct State {
    f: Frame,
    channels: usize,
    rate: u32,
    reservoir: usize,
    frame: [u8; MAX_FRAME + PAD],
    main: [u8; MAX_BACK + MAX_FRAME + PAD],
    scf_prev: [[u8; 22]; 2],
    xr: [[i32; 576]; 2],
    scratch: [i32; 576],
    overlap: [[i32; 576]; 2],
    sb: [[[i32; 32]; 18]; 2],
    synth: [Synth; 2],
}

unsafe impl Zeroable for State {}

/// MPEG-1, 2 and 2.5 audio Layer II and III, decoded to S16. One call takes one
/// whole frame, header included; Layer III carries its bit reservoir from one
/// call to the next.
pub struct Mp3Decoder {
    s: Box<State, Caps>,
}

fn parse(p: &[u8]) -> Option<Frame> {
    if p[0] != 0xff || p[1] & 0xe0 != 0xe0 {
        return None;
    }
    let version_bits = (p[1] >> 3) & 3;
    let layer_bits = (p[1] >> 1) & 3;
    let bitrate = usize::from(p[2] >> 4);
    let rate = usize::from((p[2] >> 2) & 3);
    if version_bits == 1 || (layer_bits != 1 && layer_bits != 2) {
        return None;
    }
    if bitrate == 0 || bitrate == 15 || rate == 3 {
        return None;
    }
    let version: u8 = match version_bits {
        3 => 0,
        2 => 1,
        _ => 2,
    };
    let layer = if layer_bits == 2 { 2 } else { 3 };
    if version == 2 && layer == 2 {
        return None;
    }
    let mode = p[3] >> 6;
    let kbps = KBPS[if version != 0 {
        2
    } else if layer == 2 {
        0
    } else {
        1
    }][bitrate];
    let hz = RATES[rate] >> version;
    let frames: u16 = if layer == 3 && version != 0 { 576 } else { 1152 };
    let per = u32::from(frames) / 8;
    Some(Frame {
        h: Mp3Header {
            rate: hz,
            bytes: (per * 1000 * u32::from(kbps) / hz + u32::from((p[2] >> 1) & 1)) as u16,
            frames,
            channels: if mode == MODE_MONO { 1 } else { 2 },
            layer,
        },
        kbps,
        version,
        rate_index: version * 3 + rate as u8,
        mode,
        mode_ext: (p[3] >> 4) & 3,
        crc: p[1] & 1 == 0,
    })
}

/// `None` for Layer I, free format and reserved fields.
pub fn parse_header(p: &[u8]) -> Option<Mp3Header> {
    if p.len() < MP3_HEADER_BYTES {
        return None;
    }
    parse(p).map(|f| f.h)
}

#[inline(always)]
fn to_v(v: i32) -> i32 {
    v.clamp(-SYNTH_V_MAX, SYNTH_V_MAX) * (1 << (SYNTH_V_FRAC - SYNTH_FRAC))
}

fn synth(s: &mut Synth, input: &[i32; 32], out: &mut [i16], stride: usize) {
    let mut x = [0i32; 32];
    let mut mag = 0u32;
    for (x, &v) in x.iter_mut().zip(input) {
        let v = v.wrapping_add(1 << (FRAC - SYNTH_FRAC - 1)) >> (FRAC - SYNTH_FRAC);
        *x = v.clamp(-SYNTH_IN_MAX, SYNTH_IN_MAX);
        mag |= x.unsigned_abs();
    }
    let sh = headroom_of(mag, 25);
    let mut eh = A16([0i16; 16]);
    let mut el = A16([0i16; 16]);
    let mut oh = A16([0i16; 16]);
    let mut ol = A16([0i16; 16]);
    let mut e0 = 0i32;
    for k in 0..16 {
        let a = x[k] * (1 << sh);
        let b = x[31 - k] * (1 << sh);
        e0 = e0.wrapping_add(a + b);
        split(&mut eh[k], &mut el[k], a + b);
        split(&mut oh[k], &mut ol[k], a - b);
    }
    let mut even = [0i32; 16];
    let mut odd = [0i32; 16];
    kernels::dot16(&mut even[1..], &eh, &el, &DCT16E_SPLIT, 15);
    kernels::dot16(&mut odd, &oh, &ol, &DCT16_SPLIT, 16);

    let mut xh = A16([0i16; 32]);
    let mut xl = A16([0i16; 32]);
    let round = (1 << sh) >> 1;
    for r in 0..16 {
        let e = if r != 0 { 2i32.wrapping_mul(even[r]) } else { e0 };
        let (h0, h1) = xh[2 * r..2 * r + 2].split_at_mut(1);
        let (l0, l1) = xl[2 * r..2 * r + 2].split_at_mut(1);
        split(&mut h0[0], &mut l0[0], to_v(e.wrapping_add(round) >> sh));
        split(&mut h1[0], &mut l1[0], to_v(2i32.wrapping_mul(odd[r]).wrapping_add(round) >> sh));
    }

    s.off = s.off.wrapping_sub(64) & 1023;
    let off = s.off;
    let vh = &mut s.vh[off..];
    let vl = &mut s.vl[off..];
    vh[..16].copy_from_slice(&xh[16..32]);
    vl[..16].copy_from_slice(&xl[16..32]);
    vh[16] = 0;
    vl[16] = 0;
    for i in 17..48 {
        vh[i] = xh[48 - i];
        vl[i] = xl[48 - i];
    }
    vh[48..64].copy_from_slice(&xh[..16]);
    vl[48..64].copy_from_slice(&xl[..16]);
    vh.copy_within(0..64, 1024);
    vl.copy_within(0..64, 1024);

    let mut acc = A16([0i32; 32]);
    kernels::window(&mut acc, vh, vl);
    for (j, &a) in acc.iter().enumerate() {
        let y = (a + SYNTH_FLOOR_BIAS + (1 << (SYNTH_V_FRAC - 16))) >> (SYNTH_V_FRAC - 15);
        out[j * stride] = y.clamp(-32768, 32767) as i16;
    }
}

impl State {
    fn emit(&mut self, blocks: usize, pcm: &mut [i16]) {
        let input = usize::from(self.f.h.channels);
        let out = self.channels;
        for (t, pcm) in pcm.chunks_exact_mut(32 * out).take(blocks).enumerate() {
            if input == out {
                for ch in 0..out {
                    synth(&mut self.synth[ch], &self.sb[ch][t], &mut pcm[ch..], out);
                }
            } else if out == 2 {
                synth(&mut self.synth[0], &self.sb[0][t], pcm, 2);
                for j in 0..32 {
                    pcm[2 * j + 1] = pcm[2 * j];
                }
            } else {
                let mut mix = [0i32; 32];
                for (k, m) in mix.iter_mut().enumerate() {
                    *m = ((i64::from(self.sb[0][t][k]) + i64::from(self.sb[1][t][k])) >> 1) as i32;
                }
                synth(&mut self.synth[0], &mix, pcm, 1);
            }
        }
    }
}

impl Mp3Decoder {
    /// A frame with the other channel count is mixed down or doubled. A
    /// Layer III frame whose reservoir was lost (the first after a reset)
    /// decodes as silence.
    pub fn new(config: &Mp3Config) -> Result<Self> {
        if config.channels < 1 || config.channels > 2 {
            return Err(Error::InvalidArg);
        }
        let mut s: Box<State, Caps> = heap::zeroed_box(config.caps)?;
        s.channels = usize::from(config.channels);
        Ok(Self { s })
    }

    /// The sample rate of the last decoded frame.
    pub fn rate(&self) -> u32 {
        self.s.rate
    }

    pub fn decode(&mut self, frame: &[u8], pcm: &mut [i16]) -> Result<usize> {
        if frame.len() < MP3_HEADER_BYTES {
            return Err(Error::InvalidSize);
        }
        let s = &mut *self.s;
        let Some(f) = parse(frame) else {
            let layer1 = frame[0] == 0xff && frame[1] & 0xe6 == 0xe6;
            return Err(if layer1 { Error::NotSupported } else { Error::InvalidData });
        };
        s.f = f;
        let bytes = usize::from(f.h.bytes);
        if frame.len() < bytes || bytes > MAX_FRAME {
            return Err(Error::InvalidSize);
        }
        if pcm.len() < usize::from(f.h.frames) * s.channels {
            return Err(Error::InvalidSize);
        }
        kernels::prepare();
        s.frame[..bytes].copy_from_slice(&frame[..bytes]);
        s.frame[bytes..bytes + PAD].fill(0);
        if f.h.layer == 2 {
            s.l2_decode(pcm);
        } else {
            s.l3_decode(pcm)?;
        }
        s.rate = f.h.rate;
        Ok(usize::from(f.h.frames))
    }

    pub fn reset(&mut self) {
        let s = &mut *self.s;
        s.reservoir = 0;
        s.overlap = [[0; 576]; 2];
        for synth in &mut s.synth {
            synth.vh.fill(0);
            synth.vl.fill(0);
            synth.off = 0;
        }
    }
}

impl Decoder for Mp3Decoder {
    fn decode(&mut self, frame: &[u8], pcm: PcmMut<'_>) -> Result<usize> {
        match pcm {
            PcmMut::S16(pcm) => Mp3Decoder::decode(self, frame, pcm),
            PcmMut::S32(_) => Err(Error::InvalidArg),
        }
    }

    fn reset(&mut self) {
        Mp3Decoder::reset(self);
    }

    fn max_frames(&self) -> usize {
        MP3_MAX_FRAMES
    }

    fn format(&self) -> Format {
        Format::S16
    }

    fn channels(&self) -> u8 {
        self.s.channels as u8
    }
}

fn next(s: &mut u32) -> u32 {
    *s = s.wrapping_mul(1664525).wrapping_add(1013904223);
    *s
}

#[repr(C)]
struct Selftest {
    xh: A16<[i16; 24]>,
    xl: A16<[i16; 24]>,
    vh: A16<[i16; 1088]>,
    vl: A16<[i16; 1088]>,
    a: A16<[i32; 32]>,
    b: A16<[i32; 32]>,
}

unsafe impl Zeroable for Selftest {}

/// Runs the SIMD kernels against their scalar versions on random input and
/// returns the number of mismatching calls, always 0 where there is no SIMD
/// version.
pub fn kernel_selftest(seed: u32, iterations: u32) -> Result<u32> {
    if !crate::HAVE_PIE {
        return Ok(0);
    }
    let mut seed = seed;
    let mut t: Box<Selftest, Caps> = heap::zeroed_box(Caps::DEFAULT)?;
    let t = &mut *t;
    let mut bad = 0;
    kernels::prepare();
    for _ in 0..iterations {
        let bits = 4 + next(&mut seed) % 25;
        for k in 0..24 {
            split(&mut t.xh[k], &mut t.xl[k], (next(&mut seed) as i32) >> (31 - bits));
        }
        let mut xh = A16([0i16; 16]);
        let mut xl = A16([0i16; 16]);
        xh.copy_from_slice(&t.xh[..16]);
        xl.copy_from_slice(&t.xl[..16]);
        kernels::dot16(&mut t.a[..], &xh, &xl, &DCT16E_SPLIT, 15);
        kernels::dot_scalar(&mut t.b[..], &t.xh[..], &t.xl[..], &DCT16E_SPLIT[..], 15, 16);
        bad += u32::from(t.a[..15] != t.b[..15]);
        kernels::dot16(&mut t.a[..], &xh, &xl, &DCT16_SPLIT, 16);
        kernels::dot_scalar(&mut t.b[..], &t.xh[..], &t.xl[..], &DCT16_SPLIT[..], 16, 16);
        bad += u32::from(t.a[..16] != t.b[..16]);
        kernels::dot24(&mut t.a[..], &t.xh, &t.xl, &tables::IMDCT18_SPLIT, 18);
        kernels::dot_scalar(&mut t.b[..], &t.xh[..], &t.xl[..], &tables::IMDCT18_SPLIT[..], 18, 24);
        bad += u32::from(t.a[..18] != t.b[..18]);
        for k in 0..1024 {
            split(&mut t.vh[k], &mut t.vl[k], (next(&mut seed) as i32) >> (31 - bits.min(28)));
        }
        kernels::window(&mut t.a, &t.vh[..], &t.vl[..]);
        kernels::window_scalar(&mut t.b, &t.vh[..], &t.vl[..]);
        bad += u32::from(t.a.0 != t.b.0);
    }
    Ok(bad)
}
