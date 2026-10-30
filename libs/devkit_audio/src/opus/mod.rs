/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

mod bands;
mod celt;
mod mdct;
mod rc;
mod silk;
mod silk_tables;
mod tables;

use alloc::boxed::Box;
use core::ptr;

use idf::heap::{self, Caps};

use crate::{Decoder, Error, Format, PcmMut, Result};
use bands::exp2;
use celt::Celt;
use rc::Rc;
use silk::Silk;
use tables::CELT_WINDOW;

pub const OPUS_RATE: u32 = 48000;
pub const OPUS_HEAD_BYTES: usize = 19;

// Time and frequency samples are PCM x 2^SIG_SHIFT; unit-norm band shapes are
// x 2^NORM_SHIFT.
const SIG_SHIFT: u32 = 10;
const SIG_SAT: i32 = (1 << 29) - 1;
const NORM_SHIFT: u32 = 20;
const NORM_ONE: i32 = 1 << NORM_SHIFT;

const CELT_BANDS: usize = 21;
const CELT_ALLOC_VECTORS: usize = 11;
const CELT_OVERLAP: usize = 120;
const CELT_MAX_FRAME: usize = 960;
const CELT_BUF: usize = 2048;
const MAX_PVQ_N: usize = 176;

const MAX_FRAMES: i32 = 5760;
const F20: i32 = 960;
const F5: i32 = 240;
const F2_5: i32 = 120;

const MODE_NONE: i32 = 0;
const MODE_SILK: i32 = 1;
const MODE_HYBRID: i32 = 2;
const MODE_CELT: i32 = 3;

const BW_NB: i32 = 0;
const BW_MB: i32 = 1;
const BW_SWB: i32 = 3;
const BW_FB: i32 = 4;

#[inline(always)]
fn sat_sig(v: i64) -> i32 {
    v.clamp(-i64::from(SIG_SAT), i64::from(SIG_SAT)) as i32
}

#[inline(always)]
fn mulh(a: i32, b: i32) -> i32 {
    ((i64::from(a) * i64::from(b)) >> 32) as i32
}

#[derive(Clone, Copy, Debug)]
pub struct OpusConfig<'a> {
    /// OpusHead, starting with its "OpusHead" magic.
    pub head: &'a [u8],
    pub caps: Caps,
}

/// Opus with channel mapping family 0 (mono or stereo), decoded to S16 at
/// 48 kHz. One call takes one packet, up to 120 ms. The OpusHead output gain is
/// applied; the pre-skip is left to the caller.
pub struct OpusDecoder {
    channels: usize,
    pre_skip: u16,
    gain_q16: i32,
    mode: i32,
    prev_mode: i32,
    bandwidth: i32,
    frame_size: i32,
    stream_channels: i32,
    prev_redundancy: bool,
    final_range: u32,
    celt: Celt,
    silk: Box<Silk, Caps>,
    pcm: Box<[i32], Caps>,
    transition: Box<[i32], Caps>,
    redundant: Box<[i32], Caps>,
    silk_pcm: Box<[i16], Caps>,
}

fn toc_mode(toc: u8) -> i32 {
    if toc & 0x80 != 0 {
        MODE_CELT
    } else if toc & 0x60 == 0x60 {
        MODE_HYBRID
    } else {
        MODE_SILK
    }
}

fn toc_bandwidth(toc: u8) -> i32 {
    if toc & 0x80 != 0 {
        let bw = BW_MB + i32::from((toc >> 5) & 3);
        return if bw == BW_MB { BW_NB } else { bw };
    }
    if toc & 0x60 == 0x60 {
        return if toc & 0x10 != 0 { BW_FB } else { BW_SWB };
    }
    i32::from((toc >> 5) & 3)
}

fn toc_frame_size(toc: u8) -> i32 {
    if toc & 0x80 != 0 {
        return 120 << ((toc >> 3) & 3);
    }
    if toc & 0x60 == 0x60 {
        return if toc & 0x08 != 0 { 960 } else { 480 };
    }
    let size = (toc >> 3) & 3;
    if size == 3 { 2880 } else { 480 << size }
}

fn read_size(p: &[u8], len: i32) -> Option<(i32, i32)> {
    if len < 1 {
        return None;
    }
    if p[0] < 252 {
        return Some((1, i32::from(p[0])));
    }
    if len < 2 {
        return None;
    }
    Some((2, 4 * i32::from(p[1]) + i32::from(p[0])))
}

fn parse_packet(packet: &[u8], frames: &mut [(usize, usize); 48]) -> Option<usize> {
    if packet.is_empty() {
        return None;
    }
    let frame_size = toc_frame_size(packet[0]);
    let toc = packet[0];
    let mut at = 1usize;
    let mut len = packet.len() as i32 - 1;
    let mut sizes = [0i32; 48];
    let count;
    let last;
    match toc & 3 {
        0 => {
            count = 1;
            last = len;
        }
        1 => {
            if len & 1 != 0 {
                return None;
            }
            count = 2;
            last = len / 2;
            sizes[0] = last;
        }
        2 => {
            count = 2;
            let (bytes, size) = read_size(&packet[at..], len)?;
            sizes[0] = size;
            len -= bytes;
            if size > len {
                return None;
            }
            at += bytes as usize;
            last = len - size;
        }
        _ => {
            if len < 1 {
                return None;
            }
            let ch = packet[at];
            at += 1;
            len -= 1;
            count = usize::from(ch & 0x3F);
            if count == 0 || frame_size * count as i32 > MAX_FRAMES {
                return None;
            }
            if ch & 0x40 != 0 {
                loop {
                    if len <= 0 {
                        return None;
                    }
                    let p = i32::from(packet[at]);
                    at += 1;
                    len -= 1;
                    len -= if p == 255 { 254 } else { p };
                    if p != 255 {
                        break;
                    }
                }
            }
            if len < 0 {
                return None;
            }
            if ch & 0x80 != 0 {
                let mut rest = len;
                for size in &mut sizes[..count - 1] {
                    let (bytes, s) = read_size(&packet[at..], len)?;
                    *size = s;
                    len -= bytes;
                    if s > len {
                        return None;
                    }
                    at += bytes as usize;
                    rest -= bytes + s;
                }
                if rest < 0 {
                    return None;
                }
                last = rest;
            } else {
                last = len / count as i32;
                if last * count as i32 != len {
                    return None;
                }
                sizes[..count - 1].fill(last);
            }
        }
    }
    if last > 1275 {
        return None;
    }
    sizes[count - 1] = last;
    for i in 0..count {
        frames[i] = (at, sizes[i] as usize);
        at += sizes[i] as usize;
    }
    Some(count)
}

unsafe fn smooth_fade(in1: *const i32, in2: *const i32, out: *mut i32, n: usize, channels: usize) {
    for c in 0..channels {
        for i in 0..n {
            let window = i32::from(CELT_WINDOW[i]);
            let w = i64::from(window * window >> 15);
            let at = i * channels + c;
            unsafe {
                let v = w * i64::from(*in2.add(at)) + (32767 - w) * i64::from(*in1.add(at));
                *out.add(at) = (v >> 15) as i32;
            }
        }
    }
}

fn celt_lm(frame_size: i32) -> i32 {
    let mut lm = 0;
    while (120 << lm) < frame_size {
        lm += 1;
    }
    lm
}

const END_BANDS: [u8; 5] = [13, 17, 17, 19, 21];

impl OpusDecoder {
    pub fn new(config: &OpusConfig<'_>) -> Result<Self> {
        let h = config.head;
        if h.len() < OPUS_HEAD_BYTES || &h[..8] != b"OpusHead" {
            return Err(Error::InvalidArg);
        }
        let channels = h[9];
        if h[8] & 0xF0 != 0 || h[18] != 0 || !(1..=2).contains(&channels) {
            return Err(Error::NotSupported);
        }
        let caps = config.caps;
        let channels = usize::from(channels);
        let gain_q8 = i16::from_le_bytes([h[16], h[17]]);
        let gain = exp2(f32::from(gain_q8) * (3.32192809489 / (20.0 * 256.0))) * 65536.0 + 0.5;
        let mut celt = Celt::new(channels, caps)?;
        celt.stereo_inversion = channels == 2;
        let mut silk: Box<Silk, Caps> = heap::zeroed_box(caps)?;
        silk.init(channels as i32);
        let mut d = Self {
            channels,
            pre_skip: u16::from_le_bytes([h[10], h[11]]),
            gain_q16: gain as i32,
            mode: MODE_NONE,
            prev_mode: MODE_NONE,
            bandwidth: 0,
            frame_size: 0,
            stream_channels: 0,
            prev_redundancy: false,
            final_range: 0,
            celt,
            silk,
            pcm: heap::zeroed_slice(MAX_FRAMES as usize * channels, caps)?,
            transition: heap::zeroed_slice(F5 as usize * channels, caps)?,
            redundant: heap::zeroed_slice(F5 as usize * channels, caps)?,
            silk_pcm: heap::zeroed_slice(MAX_FRAMES as usize * channels, caps)?,
        };
        d.reset();
        Ok(d)
    }

    pub fn pre_skip(&self) -> u16 {
        self.pre_skip
    }

    /// The range coder state after the last packet, as opus_demo records it.
    pub fn final_range(&self) -> u32 {
        self.final_range
    }

    pub fn reset(&mut self) {
        self.celt.reset();
        self.silk.reset();
        self.mode = MODE_NONE;
        self.prev_mode = MODE_NONE;
        self.prev_redundancy = false;
        self.frame_size = F2_5 * 8;
        self.stream_channels = self.channels as i32;
        self.final_range = 0;
    }

    unsafe fn decode_frame(&mut self, data: &[u8], pcm: *mut i32, frame_size: i32) -> i32 {
        let channels = self.channels;
        let mut len = data.len() as i32;
        let mut frame_size = frame_size;
        let mut data = Some(data);
        if len <= 1 {
            data = None;
            frame_size = frame_size.min(self.frame_size);
        }
        let mut rc = data.map(Rc::new);
        let audiosize;
        let mode;
        if data.is_some() {
            audiosize = self.frame_size;
            mode = self.mode;
        } else {
            audiosize = frame_size;
            if self.prev_mode == MODE_NONE {
                unsafe { ptr::write_bytes(pcm, 0, audiosize as usize * channels) };
                return audiosize;
            }
            mode = self.prev_mode;
        }

        if data.is_none() && frame_size > F20 && mode != MODE_SILK {
            let mut done = 0;
            while done < frame_size {
                unsafe { self.decode_frame(&[], pcm.add(done as usize * channels), F20) };
                done += F20;
            }
            return frame_size;
        }

        let trans = self.transition.as_mut_ptr();
        let mut transition = false;
        if data.is_some()
            && self.prev_mode != MODE_NONE
            && ((mode == MODE_CELT && self.prev_mode != MODE_CELT && !self.prev_redundancy)
                || (mode != MODE_CELT && self.prev_mode == MODE_CELT))
        {
            transition = true;
            if mode == MODE_CELT {
                unsafe { self.decode_frame(&[], trans, F5.min(audiosize)) };
            }
        }
        frame_size = audiosize;
        let samples = frame_size as usize * channels;

        let silk_pcm = self.silk_pcm.as_mut_ptr();
        if mode != MODE_CELT {
            if self.prev_mode == MODE_CELT {
                self.silk.reset();
            }
            let payload_ms = (audiosize / 48).max(10);
            let mut internal_khz = 16;
            if mode == MODE_SILK {
                if self.bandwidth == BW_NB {
                    internal_khz = 8;
                }
                if self.bandwidth == BW_MB {
                    internal_khz = 12;
                }
            }
            let mut done = 0;
            loop {
                let n = unsafe {
                    self.silk.decode(rc.as_mut(), done == 0, internal_khz, self.stream_channels as usize, payload_ms, silk_pcm.add(done as usize * channels))
                };
                if n <= 0 {
                    if data.is_some() {
                        return -1;
                    }
                    unsafe { ptr::write_bytes(silk_pcm.add(done as usize * channels), 0, (frame_size - done) as usize * channels) };
                    break;
                }
                done += n;
                if done >= frame_size {
                    break;
                }
            }
        }

        let mut redundancy = false;
        let mut celt_to_silk = false;
        let mut redundancy_bytes = 0i32;
        if let Some(r) = rc.as_mut() {
            let hybrid = mode == MODE_HYBRID;
            if mode != MODE_CELT && r.tell() + 17 + 20 * i32::from(hybrid) <= 8 * len {
                redundancy = !hybrid || r.bit_logp(12);
                if redundancy {
                    celt_to_silk = r.bit_logp(1);
                    redundancy_bytes = if hybrid { r.uint(256) as i32 + 2 } else { len - ((r.tell() + 7) >> 3) };
                    len -= redundancy_bytes;
                    if len * 8 < r.tell() {
                        len = 0;
                        redundancy_bytes = 0;
                        redundancy = false;
                    }
                    r.storage = r.storage.wrapping_sub(redundancy_bytes as u32);
                }
            }
        }
        let start_band = if mode != MODE_CELT { 17 } else { 0 };
        let end_band = END_BANDS[self.bandwidth as usize];
        self.celt.end = end_band;
        self.celt.stream_channels = self.stream_channels as u8;

        if redundancy {
            transition = false;
        }
        if transition && mode != MODE_CELT {
            unsafe { self.decode_frame(&[], trans, F5.min(audiosize)) };
        }

        let red = self.redundant.as_mut_ptr();
        let mut redundant_rng = 0;
        let red_data = |len: i32| data.map(|d| &d[len as usize..len as usize + redundancy_bytes as usize]);
        if redundancy && celt_to_silk {
            let mut rrc = Rc::new(red_data(len).unwrap_or(&[]));
            self.celt.start = 0;
            unsafe { self.celt.decode(Some(&mut rrc), redundancy_bytes as u32, 1, red) };
            redundant_rng = rrc.rng;
        }

        self.celt.start = start_band;
        if mode != MODE_SILK {
            let celt_frame = F20.min(frame_size);
            if mode != self.prev_mode && self.prev_mode != MODE_NONE && !self.prev_redundancy {
                self.celt.reset();
            }
            self.celt.stream_channels = self.stream_channels as u8;
            self.celt.end = end_band;
            self.celt.start = start_band;
            unsafe { self.celt.decode(rc.as_mut(), len as u32, celt_lm(celt_frame), pcm) };
        } else {
            unsafe { ptr::write_bytes(pcm, 0, samples) };
            if self.prev_mode == MODE_HYBRID && !(redundancy && celt_to_silk && self.prev_redundancy) {
                let mut src = Rc::new(&[0xFF, 0xFF]);
                self.celt.start = 0;
                unsafe { self.celt.decode(Some(&mut src), 2, 0, pcm) };
            }
        }

        if mode != MODE_CELT {
            for i in 0..samples {
                unsafe {
                    let silk = i64::from(i32::from(*silk_pcm.add(i)) << SIG_SHIFT);
                    *pcm.add(i) = sat_sig(i64::from(*pcm.add(i)) + silk);
                }
            }
        }

        let f25 = F2_5 as usize;
        unsafe {
            if redundancy && !celt_to_silk {
                let mut rrc = Rc::new(red_data(len).unwrap_or(&[]));
                self.celt.reset();
                self.celt.start = 0;
                self.celt.stream_channels = self.stream_channels as u8;
                self.celt.end = end_band;
                self.celt.decode(Some(&mut rrc), redundancy_bytes as u32, 1, red);
                redundant_rng = rrc.rng;
                let tail = pcm.add(channels * (frame_size as usize - f25));
                smooth_fade(tail, red.add(channels * f25), tail, f25, channels);
            }
            if redundancy && celt_to_silk {
                ptr::copy_nonoverlapping(red, pcm, f25 * channels);
                let mid = pcm.add(channels * f25);
                smooth_fade(red.add(channels * f25), mid, mid, f25, channels);
            }
            if transition {
                if audiosize >= F5 {
                    ptr::copy_nonoverlapping(trans, pcm, f25 * channels);
                    let mid = pcm.add(channels * f25);
                    smooth_fade(trans.add(channels * f25), mid, mid, f25, channels);
                } else {
                    smooth_fade(trans, pcm, pcm, f25, channels);
                }
            }
        }

        self.final_range = if len <= 1 { 0 } else { rc.as_ref().map_or(0, |r| r.rng) ^ redundant_rng };
        self.prev_mode = mode;
        self.prev_redundancy = redundancy && !celt_to_silk;
        audiosize
    }

    pub fn decode(&mut self, packet: &[u8], pcm: &mut [i16]) -> Result<usize> {
        let channels = self.channels;
        if pcm.len() < MAX_FRAMES as usize * channels {
            return Err(Error::InvalidSize);
        }
        let out = self.pcm.as_mut_ptr();
        let mut total = 0i32;
        if packet.is_empty() {
            total = unsafe { self.decode_frame(&[], out, self.frame_size) };
        } else {
            let mut frames = [(0usize, 0usize); 48];
            let count = parse_packet(packet, &mut frames).ok_or(Error::InvalidData)?;
            self.mode = toc_mode(packet[0]);
            self.bandwidth = toc_bandwidth(packet[0]);
            self.frame_size = toc_frame_size(packet[0]);
            self.stream_channels = if packet[0] & 4 != 0 { 2 } else { 1 };
            for &(at, size) in &frames[..count] {
                let n = unsafe { self.decode_frame(&packet[at..at + size], out.add(total as usize * channels), self.frame_size) };
                if n < 0 {
                    return Err(Error::InvalidData);
                }
                total += n;
            }
        }
        const ROUND: i64 = 1 << (15 + SIG_SHIFT);
        let samples = total as usize * channels;
        for (o, &v) in pcm[..samples].iter_mut().zip(&self.pcm[..samples]) {
            let v = (i64::from(v) * i64::from(self.gain_q16) + ROUND) >> (16 + SIG_SHIFT);
            *o = v.clamp(-32768, 32767) as i16;
        }
        Ok(total as usize)
    }
}

impl Decoder for OpusDecoder {
    fn decode(&mut self, frame: &[u8], pcm: PcmMut<'_>) -> Result<usize> {
        match pcm {
            PcmMut::S16(pcm) => OpusDecoder::decode(self, frame, pcm),
            PcmMut::S32(_) => Err(Error::InvalidArg),
        }
    }

    fn reset(&mut self) {
        OpusDecoder::reset(self);
    }

    fn max_frames(&self) -> usize {
        MAX_FRAMES as usize
    }

    fn format(&self) -> Format {
        Format::S16
    }

    fn channels(&self) -> u8 {
        self.channels as u8
    }
}
