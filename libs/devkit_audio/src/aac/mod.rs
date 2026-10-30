/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

mod filterbank;
mod ics;
pub mod kernels;
mod ps;
mod sbr;
mod sbr_hf;
mod tables;

use alloc::boxed::Box;
use alloc::vec::Vec;

use devkit::A16;
use idf::heap::{self, Caps};

use crate::bits::Bits;
use crate::{Decoder, Error, Format, HAVE_PIE, PcmMut, Result};
pub use kernels::kernel_selftest;
use tables::HUFF_TABLES;

const FRAME_LEN: usize = 1024;
const MAX_SFB: usize = 51;
const MAX_BYTES: usize = 8192;
const BUF_PAD: usize = 8;

const LONG_START: u8 = 1;
const EIGHT_SHORT: u8 = 2;
const LONG_STOP: u8 = 3;

const ID_SCE: u32 = 0;
const ID_CPE: u32 = 1;
const ID_DSE: u32 = 4;
const ID_PCE: u32 = 5;
const ID_FIL: u32 = 6;
const ID_END: u32 = 7;

const AAC_HCB_SF: usize = 11;
const HUFF_BASE_PS: usize = 22;

const RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum HeMode {
    /// Core only: an HE-AAC stream plays at its core rate, without the high band.
    #[default]
    Off,
    /// SBR; a PS stream plays as mono.
    V1,
    /// SBR and parametric stereo.
    V2,
}

#[derive(Clone, Copy, Debug)]
pub struct AacConfig<'a> {
    /// The AudioSpecificConfig, or the first frame when `adts`.
    pub asc: &'a [u8],
    /// Every frame starts with an ADTS header.
    pub adts: bool,
    pub he: HeMode,
    pub caps: Caps,
    /// For the 12 KB work buffer every stage touches; `None` or out of memory
    /// falls back to `caps`.
    pub scratch_caps: Option<Caps>,
    /// Cycle counter for [`AacDecoder::take_profile`].
    pub clock: Option<fn() -> u32>,
}

/// Index into [`AacDecoder::take_profile`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub enum ProfileStage {
    Core,
    Filterbank,
    QmfAnalysis,
    Sbr,
    Ps,
    QmfSynthesis,
}

pub const PROFILE_STAGES: usize = 6;

#[inline(always)]
fn huff(b: &mut Bits<'_>, table: usize) -> i32 {
    let h = &HUFF_TABLES[table];
    let w = b.show();
    let mut bits = u32::from(h.root_bits);
    let mut base = 0;
    let mut e = u32::from(unsafe { *h.lut.get_unchecked((w >> (32 - bits)) as usize) });
    while e >> 15 != 0 {
        base += bits;
        bits = (e >> 12) & 7;
        let at = (e & 0xfff) + ((w << base) >> (32 - bits));
        e = u32::from(unsafe { *h.lut.get_unchecked(at as usize) });
    }
    b.pos += base + (e >> 9);
    (e & 0x1ff) as i32 + i32::from(h.offset)
}

#[derive(Clone, Copy, Default)]
struct TnsWin {
    n_filt: u8,
    coef_res: bool,
    length: [u8; 3],
    order: [u8; 3],
    direction: [bool; 3],
    compress: [bool; 3],
    coef: [[u8; 12]; 3],
}

struct Ics {
    window_sequence: u8,
    window_shape: bool,
    max_sfb: u8,
    num_windows: u8,
    num_groups: u8,
    group_len: [u8; 8],
    num_swb: u8,
    swb_offset: &'static [u16],
    global_gain: u8,
    cb: [[u8; MAX_SFB + 1]; 8],
    sf: [[i16; MAX_SFB + 1]; 8],
    pulse: bool,
    pulse_n: u8,
    pulse_start: u8,
    pulse_offset: [u8; 4],
    pulse_amp: [u8; 4],
    tns: bool,
    tns_win: [TnsWin; 8],
}

impl Ics {
    fn new() -> Self {
        Self {
            window_sequence: 0,
            window_shape: false,
            max_sfb: 0,
            num_windows: 0,
            num_groups: 0,
            group_len: [0; 8],
            num_swb: 0,
            swb_offset: &[],
            global_gain: 0,
            cb: [[0; MAX_SFB + 1]; 8],
            sf: [[0; MAX_SFB + 1]; 8],
            pulse: false,
            pulse_n: 0,
            pulse_start: 0,
            pulse_offset: [0; 4],
            pulse_amp: [0; 4],
            tns: false,
            tns_win: [TnsWin::default(); 8],
        }
    }
}

struct Chan {
    ics: Ics,
    spec: Box<A16<[i32; FRAME_LEN]>, Caps>,
    overlap: Box<[i32; FRAME_LEN], Caps>,
    time: Option<Box<A16<[i32; FRAME_LEN]>, Caps>>,
    prev_shape: bool,
}

struct Dec {
    channels: u8,
    max_frames: usize,
    sf_index: usize,
    core_channels: usize,
    out_rate: u32,
    adts: bool,
    ps_on: bool,
    ms_used: [[u8; MAX_SFB + 1]; 8],
    ms_present: u8,
    common_window: bool,
    ch: Vec<Chan, Caps>,
    noise_seed: u32,
    noise_seeds: [[u32; MAX_SFB + 1]; 8],
    scratch: Box<A16<[i32; 3 * FRAME_LEN]>, Caps>,
    sbr: Option<Box<sbr::State, Caps>>,
    tables: kernels::Tables,
    _ram_tables: Option<Box<kernels::RamTables, Caps>>,
    clock: Option<fn() -> u32>,
    prof_mark: u32,
    prof: [u64; PROFILE_STAGES],
}

unsafe impl Send for Dec {}

/// AAC-LC, mono or stereo, S16 PCM, with optional SBR and parametric stereo.
/// The output rate and channel count are fixed at creation. With SBR enabled,
/// a core rate of 24 kHz or less is decoded at twice the rate even when the
/// stream never carries SBR data, and V2 turns a mono core into stereo.
pub struct AacDecoder {
    d: Dec,
    frame: Box<[u8; MAX_BYTES + BUF_PAD], Caps>,
}

struct Asc {
    aot: i32,
    sf_index: i32,
    rate: u32,
    channels: i32,
    sbr: i32,
    ps: i32,
    ext_rate: u32,
}

fn rate_index(rate: u32) -> i32 {
    const LOWER: [u32; 12] = [92017, 75132, 55426, 46009, 37566, 27713, 23004, 18783, 13856, 11502, 9391, 0];
    LOWER.iter().position(|&l| rate >= l).map_or(11, |i| i as i32)
}

fn read_rate(b: &mut Bits<'_>) -> (i32, u32) {
    let index = b.get(4) as i32;
    if index == 15 {
        let rate = b.get(24);
        (rate_index(rate), rate)
    } else {
        (index, if index < 13 { RATES[index as usize] } else { 0 })
    }
}

fn read_aot(b: &mut Bits<'_>) -> i32 {
    let aot = b.get(5) as i32;
    if aot == 31 { 32 + b.get(6) as i32 } else { aot }
}

fn parse_pce(b: &mut Bits<'_>) -> i32 {
    b.get(4 + 2 + 4);
    let front = b.get(4);
    let side = b.get(4);
    let back = b.get(4);
    let lfe = b.get(2);
    let assoc = b.get(3);
    let cc = b.get(4);
    if b.bit() {
        b.get(4);
    }
    if b.bit() {
        b.get(4);
    }
    if b.bit() {
        b.get(3);
    }
    let mut channels = 0;
    for _ in 0..front + side + back {
        channels += if b.bit() { 2 } else { 1 };
        b.get(4);
    }
    b.pos += 4 * (lfe + assoc) + 5 * cc;
    b.pos = (b.pos + 7) & !7;
    let comment = b.get(8);
    b.pos += 8 * comment;
    if front + side + back != 1 || lfe != 0 || cc != 0 {
        return 0;
    }
    channels
}

fn parse_asc(asc: &[u8]) -> Result<Asc> {
    if asc.is_empty() || asc.len() > 64 {
        return Err(Error::InvalidArg);
    }
    let mut buf = [0u8; 64 + BUF_PAD];
    buf[..asc.len()].copy_from_slice(asc);
    let mut b = Bits::new(&buf, 0, asc.len() as u32 * 8);
    let mut cfg = Asc { aot: 0, sf_index: 0, rate: 0, channels: 0, sbr: -1, ps: -1, ext_rate: 0 };
    cfg.aot = read_aot(&mut b);
    (cfg.sf_index, cfg.rate) = read_rate(&mut b);
    cfg.channels = b.get(4) as i32;
    if cfg.aot == 5 || cfg.aot == 29 {
        cfg.sbr = 1;
        if cfg.aot == 29 {
            cfg.ps = 1;
        }
        cfg.ext_rate = read_rate(&mut b).1;
        cfg.aot = read_aot(&mut b);
    }
    if cfg.aot != 2 {
        return Err(Error::NotSupported);
    }
    if b.bit() {
        return Err(Error::NotSupported);
    }
    if b.bit() {
        b.get(14);
    }
    if b.bit() {
        return Err(Error::NotSupported);
    }
    if cfg.channels == 0 {
        cfg.channels = parse_pce(&mut b);
    }
    if cfg.sbr < 0 && b.left() >= 16 && b.get(11) == 0x2b7 && read_aot(&mut b) == 5 {
        cfg.sbr = i32::from(b.bit());
        if cfg.sbr != 0 {
            cfg.ext_rate = read_rate(&mut b).1;
            if b.left() >= 12 && b.get(11) == 0x548 {
                cfg.ps = i32::from(b.bit());
            }
        }
    }
    if b.overrun() {
        return Err(Error::InvalidSize);
    }
    Ok(cfg)
}

struct AdtsHeader {
    profile: u8,
    sf_index: u8,
    channels: u8,
    frame_len: u32,
    header_len: u32,
    blocks: u8,
}

fn parse_adts(p: &[u8]) -> Option<AdtsHeader> {
    if p.len() < 7 || p[0] != 0xFF || p[1] & 0xF6 != 0xF0 {
        return None;
    }
    let h = AdtsHeader {
        profile: p[2] >> 6,
        sf_index: (p[2] >> 2) & 0xF,
        channels: (p[2] & 1) << 2 | p[3] >> 6,
        frame_len: u32::from(p[3] & 3) << 11 | u32::from(p[4]) << 3 | u32::from(p[5] >> 5),
        header_len: if p[1] & 1 != 0 { 7 } else { 9 },
        blocks: p[6] & 3,
    };
    if h.frame_len >= h.header_len { Some(h) } else { None }
}

/// The length of the ADTS frame at `data`, `None` when it does not start with one.
pub fn adts_frame_len(data: &[u8]) -> Option<usize> {
    parse_adts(data).map(|h| h.frame_len as usize)
}

impl Dec {
    #[inline]
    fn prof_start(&mut self) {
        if let Some(clock) = self.clock {
            self.prof_mark = clock();
        }
    }

    #[inline]
    fn prof_stage(&mut self, stage: ProfileStage) {
        if let Some(clock) = self.clock {
            let now = clock();
            self.prof[stage as usize] += u64::from(now.wrapping_sub(self.prof_mark));
            self.prof_mark = now;
        }
    }

    fn parse_cpe(&mut self, b: &mut Bits<'_>) -> Result<()> {
        let (left, right) = self.ch.split_at_mut(1);
        let (cl, cr) = (&mut left[0], &mut right[0]);
        b.get(4);
        self.common_window = b.bit();
        self.ms_present = 0;
        self.ms_used = [[0; MAX_SFB + 1]; 8];
        if self.common_window {
            ics::ics_info(self.sf_index, b, &mut cl.ics)?;
            let (l, r) = (&cl.ics, &mut cr.ics);
            r.window_sequence = l.window_sequence;
            r.window_shape = l.window_shape;
            r.max_sfb = l.max_sfb;
            r.num_windows = l.num_windows;
            r.num_groups = l.num_groups;
            r.group_len = l.group_len;
            r.num_swb = l.num_swb;
            r.swb_offset = l.swb_offset;
            self.ms_present = b.get(2) as u8;
            if self.ms_present == 3 {
                return Err(Error::InvalidData);
            }
            for g in 0..usize::from(l.num_groups) {
                for sfb in 0..usize::from(l.max_sfb) {
                    let used = self.ms_present == 2 || (self.ms_present == 1 && b.bit());
                    self.ms_used[g][sfb] = u8::from(used);
                }
            }
        }
        ics::ics_parse(self.sf_index, b, cl, self.common_window)?;
        ics::ics_parse(self.sf_index, b, cr, self.common_window)
    }

    fn parse_fill(&mut self, b: &mut Bits<'_>, element: i32) {
        let mut count = b.get(4);
        if count == 15 {
            count = (count + b.get(8)).wrapping_sub(1);
        }
        if count == 0 {
            return;
        }
        let bits = count.wrapping_mul(8);
        let start = b.pos;
        let kind = b.get(4);
        if (kind == 13 || kind == 14) && element >= 0 {
            if let Some(s) = &mut self.sbr {
                let _ = s.parse(b, element as u32, self.core_channels, bits.wrapping_sub(4), kind == 14);
            }
        }
        b.pos = start.wrapping_add(bits);
    }

    fn raw_data_block(&mut self, b: &mut Bits<'_>) -> Result<()> {
        let mut element = -1;
        loop {
            let id = b.get(3);
            match id {
                ID_END => break,
                ID_SCE => {
                    if self.core_channels != 1 || element >= 0 {
                        return Err(Error::NotSupported);
                    }
                    b.get(4);
                    self.common_window = false;
                    self.ms_used = [[0; MAX_SFB + 1]; 8];
                    ics::ics_parse(self.sf_index, b, &mut self.ch[0], false)?;
                    element = ID_SCE as i32;
                }
                ID_CPE => {
                    if self.core_channels != 2 || element >= 0 {
                        return Err(Error::NotSupported);
                    }
                    self.parse_cpe(b)?;
                    element = ID_CPE as i32;
                }
                ID_FIL => self.parse_fill(b, element),
                ID_DSE => {
                    b.get(4);
                    let align = b.bit();
                    let mut count = b.get(8);
                    if count == 255 {
                        count += b.get(8);
                    }
                    if align {
                        b.pos = (b.pos + 7) & !7;
                    }
                    b.pos += count * 8;
                }
                ID_PCE => {
                    if parse_pce(b) as usize != self.core_channels {
                        return Err(Error::NotSupported);
                    }
                }
                _ => return Err(Error::NotSupported),
            }
            if b.overrun() {
                return Err(Error::InvalidSize);
            }
        }
        if element >= 0 { Ok(()) } else { Err(Error::InvalidSize) }
    }
}

impl AacDecoder {
    pub fn new(config: &AacConfig<'_>) -> Result<Self> {
        let mut cfg = Asc { aot: 0, sf_index: 0, rate: 0, channels: 0, sbr: -1, ps: -1, ext_rate: 0 };
        if config.adts {
            let h = parse_adts(config.asc).ok_or(Error::InvalidArg)?;
            if h.profile != 1 {
                return Err(Error::NotSupported);
            }
            cfg.aot = 2;
            cfg.sf_index = i32::from(h.sf_index);
            cfg.rate = if h.sf_index < 13 { RATES[usize::from(h.sf_index)] } else { 0 };
            cfg.channels = i32::from(h.channels);
        } else {
            cfg = parse_asc(config.asc)?;
        }
        if cfg.sf_index > 12 || cfg.rate == 0 || !(1..=2).contains(&cfg.channels) {
            return Err(Error::NotSupported);
        }

        let caps = config.caps;
        let mut sbr_on = false;
        if config.he != HeMode::Off {
            if cfg.sbr == 1 {
                sbr_on = cfg.ext_rate == 0 || cfg.ext_rate == 2 * cfg.rate;
            } else {
                sbr_on = cfg.sbr < 0 && cfg.rate <= 24000;
            }
        }
        let ps_on = sbr_on && config.he == HeMode::V2 && cfg.channels == 1 && cfg.ps != 0;
        let out_rate = if sbr_on { 2 * cfg.rate } else { cfg.rate };
        let channels = if ps_on { 2 } else { cfg.channels as u8 };
        let core_channels = cfg.channels as usize;

        let scratch = match config.scratch_caps.map(heap::zeroed_box) {
            Some(Ok(scratch)) => scratch,
            _ => heap::zeroed_box(caps)?,
        };
        let mut ch = heap::try_vec(core_channels, caps)?;
        for _ in 0..core_channels {
            ch.push(Chan {
                ics: Ics::new(),
                spec: heap::zeroed_box(caps)?,
                overlap: heap::zeroed_box(caps)?,
                time: if sbr_on { None } else { Some(heap::zeroed_box(caps)?) },
                prev_shape: false,
            });
        }
        let sbr = if sbr_on { Some(sbr::State::new(out_rate, core_channels, usize::from(channels), ps_on, caps)?) } else { None };
        let mut ram_tables = None;
        let mut tables = kernels::Tables::rom();
        if HAVE_PIE && sbr_on {
            let mut ram: Box<kernels::RamTables, Caps> = heap::zeroed_box(caps)?;
            tables = kernels::Tables::ram(&mut ram);
            ram_tables = Some(ram);
        }
        Ok(Self {
            d: Dec {
                channels,
                max_frames: if sbr_on { 2 * FRAME_LEN } else { FRAME_LEN },
                sf_index: cfg.sf_index as usize,
                core_channels,
                out_rate,
                adts: config.adts,
                ps_on,
                ms_used: [[0; MAX_SFB + 1]; 8],
                ms_present: 0,
                common_window: false,
                ch,
                noise_seed: 0x1f2e3d4c,
                noise_seeds: [[0; MAX_SFB + 1]; 8],
                scratch,
                sbr,
                tables,
                _ram_tables: ram_tables,
                clock: config.clock,
                prof_mark: 0,
                prof: [0; PROFILE_STAGES],
            },
            frame: heap::zeroed_box(caps)?,
        })
    }

    pub fn rate(&self) -> u32 {
        self.d.out_rate
    }

    /// Clock cycles spent per stage since the last call; `None` without a clock.
    pub fn take_profile(&mut self) -> Option<[u64; PROFILE_STAGES]> {
        self.d.clock?;
        Some(core::mem::replace(&mut self.d.prof, [0; PROFILE_STAGES]))
    }

    pub fn decode(&mut self, frame: &[u8], pcm: &mut [i16]) -> Result<usize> {
        let d = &mut self.d;
        if frame.len() > MAX_BYTES {
            return Err(Error::InvalidSize);
        }
        if pcm.len() < d.max_frames * usize::from(d.channels) {
            return Err(Error::InvalidSize);
        }
        let len = frame.len();
        self.frame[..len].copy_from_slice(frame);
        self.frame[len..len + BUF_PAD].fill(0);
        let mut b = Bits::new(&self.frame[..], 0, len as u32 * 8);
        if d.adts {
            let h = match parse_adts(frame) {
                Some(h) if h.frame_len as usize <= len => h,
                _ => return Err(Error::InvalidSize),
            };
            if h.blocks != 0 || h.profile != 1 || usize::from(h.sf_index) != d.sf_index {
                return Err(Error::NotSupported);
            }
            b.pos = h.header_len * 8;
            b.end = h.frame_len * 8;
        }
        kernels::prepare();
        d.prof_start();
        if let Some(s) = &mut d.sbr {
            s.frame_start();
        }
        d.raw_data_block(&mut b)?;

        d.spectrum();
        d.prof_stage(ProfileStage::Core);
        if d.sbr.is_some() {
            d.sbr_apply(pcm);
            return Ok(2 * FRAME_LEN);
        }
        let scratch = d.scratch.as_mut_ptr();
        let tables = d.tables;
        for c in &mut d.ch {
            let time = c.time.as_mut().map_or(core::ptr::null_mut(), |t| t.as_mut_ptr());
            unsafe { filterbank::filterbank(c, &tables, scratch, time) };
        }
        d.prof_stage(ProfileStage::Filterbank);
        let left = d.ch[0].time.as_ref().map_or(core::ptr::null(), |t| t.as_ptr());
        let right = d.ch.get(1).and_then(|c| c.time.as_ref()).map_or(core::ptr::null(), |t| t.as_ptr());
        unsafe { kernels::pack(pcm.as_mut_ptr(), left, right, FRAME_LEN as i32, 3) };
        Ok(FRAME_LEN)
    }

    pub fn reset(&mut self) {
        let d = &mut self.d;
        for c in &mut d.ch {
            c.overlap.fill(0);
            c.prev_shape = false;
        }
        d.noise_seed = 0x1f2e3d4c;
        if let Some(s) = &mut d.sbr {
            s.reset();
        }
    }
}

impl Decoder for AacDecoder {
    fn decode(&mut self, frame: &[u8], pcm: PcmMut<'_>) -> Result<usize> {
        match pcm {
            PcmMut::S16(pcm) => AacDecoder::decode(self, frame, pcm),
            PcmMut::S32(_) => Err(Error::InvalidArg),
        }
    }

    fn reset(&mut self) {
        AacDecoder::reset(self);
    }

    fn max_frames(&self) -> usize {
        self.d.max_frames
    }

    fn format(&self) -> Format {
        Format::S16
    }

    fn channels(&self) -> u8 {
        self.d.channels
    }
}

