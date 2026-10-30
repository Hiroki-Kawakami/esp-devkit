/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#[path = "lossy_tables.rs"]
mod tables;

use alloc::boxed::Box;
use core::ptr;

use devkit::{A16, Zeroable};
use idf::heap::Caps;

use super::{Cursor, Source};
use crate::mem::{Buffer, buffer, state};
use crate::{Error, Result};
use tables::{AC_TABLE, BMODE_PROBS, COEFF_PROBS, COEFF_UPDATE_PROBS, DC_TABLE};

const BPS: usize = 32;
const IBPS: isize = BPS as isize;
const Y_OFF: usize = BPS + 8;
const U_OFF: usize = Y_OFF + BPS * 17;
const V_OFF: usize = U_OFF + 16;
const WORK_SIZE: usize = BPS * 26;

const PART0_WINDOW: usize = 8192;
const TOKEN_WINDOW: usize = 32768;

// The loop filter reads four rows above the macroblock row, and output trails the
// decoder by up to seven luma rows.
const CACHE_ABOVE: usize = 8;

const B_DC_PRED: u8 = 0;
const B_TM_PRED: u8 = 1;
const B_VE_PRED: u8 = 2;
const B_HE_PRED: u8 = 3;
const B_RD_PRED: u8 = 4;
const B_VR_PRED: u8 = 5;
const B_LD_PRED: u8 = 6;
const B_VL_PRED: u8 = 7;
const B_HD_PRED: u8 = 8;
const B_HU_PRED: u8 = 9;

const DC_PRED: u8 = 0;
const TM_PRED: u8 = 1;
const V_PRED: u8 = 2;
const H_PRED: u8 = 3;
const DC_NOTOP: u8 = 4;
const DC_NOLEFT: u8 = 5;
const DC_NOTOPLEFT: u8 = 6;

const ZIGZAG: [u8; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];
const BANDS: [u8; 17] = [0, 1, 2, 3, 6, 4, 5, 6, 6, 6, 6, 6, 6, 6, 6, 7, 0];
const CAT3456: [&[u8]; 4] = [
    &[173, 148, 140],
    &[176, 155, 140, 135],
    &[180, 157, 141, 134, 130],
    &[254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129],
];

// Left shift that brings a range back to 128..=255. A table, because these chips
// have no count-leading-zeros instruction and this runs once per decoded bit.
const NORM: [u8; 256] = {
    let mut t = [0; 256];
    let mut i = 1;
    while i < 256 {
        t[i] = (i as u32).leading_zeros() as u8 - 24;
        i += 1;
    }
    t
};

type BandProbs = [[u8; 11]; 3];
type TypeProbs = [BandProbs; 8];

/// Running past the end feeds one byte of zeros and then raises `eof`, the point
/// at which libwebp gives up on the stream.
struct BoolDecoder {
    cursor: Cursor,
    pos: usize,
    len: usize,
    value: u32,
    bits: i32,
    range: u32,
    eof: bool,
}

impl BoolDecoder {
    fn new(cursor: Cursor) -> Self {
        Self { cursor, pos: 0, len: 0, value: 0, bits: -8, range: 254, eof: false }
    }

    #[inline(never)]
    fn load(&mut self, src: &mut Source) {
        if self.pos == self.len {
            self.len = self.cursor.refill(src);
            self.pos = 0;
        }
        let buf = self.cursor.buf.as_ptr();
        if self.len - self.pos >= 3 {
            let p = unsafe { buf.add(self.pos).cast::<[u8; 3]>().read() };
            self.value = (self.value << 24) | (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2]);
            self.pos += 3;
            self.bits += 24;
        } else if self.pos < self.len {
            self.value = (self.value << 8) | u32::from(unsafe { *buf.add(self.pos) });
            self.pos += 1;
            self.bits += 8;
        } else if !self.eof {
            self.value <<= 8;
            self.bits += 8;
            self.eof = true;
        } else {
            self.bits = 0;
        }
    }

    fn start(&mut self, src: &mut Source) {
        self.pos = 0;
        self.len = 0;
        self.value = 0;
        self.bits = -8;
        self.range = 254;
        self.eof = false;
        self.load(src);
    }

    #[inline(always)]
    fn bit(&mut self, src: &mut Source, prob: u8) -> u32 {
        let mut range = self.range;
        if self.bits < 0 {
            self.load(src);
        }
        let pos = self.bits;
        let split = (range * u32::from(prob)) >> 8;
        let bit;
        if (self.value >> pos) > split {
            range -= split;
            self.value = self.value.wrapping_sub((split + 1) << pos);
            bit = 1;
        } else {
            range = split + 1;
            bit = 0;
        }
        // range is 1..=255 here.
        let shift = u32::from(unsafe { *NORM.get_unchecked(range as usize) });
        range <<= shift;
        self.bits -= shift as i32;
        self.range = range - 1;
        bit
    }

    #[inline(always)]
    fn signed(&mut self, src: &mut Source, v: i32) -> i32 {
        if self.bits < 0 {
            self.load(src);
        }
        let pos = self.bits;
        let split = self.range >> 1;
        let value = self.value >> pos;
        let mask = (split.wrapping_sub(value) as i32) >> 31;
        self.bits -= 1;
        self.range = self.range.wrapping_add(mask as u32) | 1;
        self.value = self.value.wrapping_sub(((split + 1) & mask as u32) << pos);
        (v ^ mask) - mask
    }

    fn flag(&mut self, src: &mut Source) -> bool {
        self.bit(src, 0x80) != 0
    }

    fn value(&mut self, src: &mut Source, bits: u32) -> u32 {
        (0..bits).rev().fold(0, |v, b| v | (self.bit(src, 0x80) << b))
    }

    fn signed_value(&mut self, src: &mut Source, bits: u32) -> i32 {
        let value = self.value(src, bits) as i32;
        if self.flag(src) { -value } else { value }
    }
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
struct FilterInfo {
    limit: u8,
    ilevel: u8,
    inner: u8,
    hev: u8,
}

unsafe impl Zeroable for FilterInfo {}

#[derive(Default)]
struct Macroblock {
    segment: u8,
    skip: bool,
    is_i4x4: bool,
    uvmode: u8,
    imodes: [u8; 16],
}

const Y1: usize = 0;
const Y2: usize = 1;
const UV: usize = 2;

#[repr(C)]
struct State {
    probs: [TypeProbs; 4],
    dq: [[[i32; 2]; 3]; 4],
    fstrengths: [[FilterInfo; 2]; 4],
    seg_probs: [u8; 3],
    coeffs: A16<[i16; 384]>,
    kinds: [u8; 24],
    work: A16<[u8; WORK_SIZE]>,
}

unsafe impl Zeroable for State {}

/// A VP8 key frame (RFC 6386).
pub(crate) struct Vp8 {
    width: u32,
    height: u32,
    mb_w: usize,
    mb_h: usize,

    br: BoolDecoder,
    parts: [Option<BoolDecoder>; 8],
    num_parts: usize,

    update_map: bool,
    filter_type: u8,
    use_skip: bool,
    skip_p: u8,
    s: Box<State, Caps>,

    intra_t: Buffer,
    intra_l: [u8; 4],
    nz_t: Buffer,
    nz_dc_t: Buffer,
    nz_l: u8,
    nz_dc_l: u8,
    top: Buffer,
    finfo: Buffer<FilterInfo>,

    cache_y: Buffer,
    cache_u: Buffer,
    cache_v: Buffer,
    y_stride: usize,
    uv_stride: usize,
    mb_y: usize,
    y_final: usize,
    uv_final: usize,
    out_y: usize,
}

fn clip(v: i32, max: i32) -> usize {
    v.clamp(0, max) as usize
}

fn parse_quant(s: &mut State, br: &mut BoolDecoder, src: &mut Source, quantizer: &[i32; 4], use_segment: bool, absolute: bool) {
    let base_q = br.value(src, 7) as i32;
    let mut delta = [0; 5];
    for d in &mut delta {
        if br.flag(src) {
            *d = br.signed_value(src, 4);
        }
    }
    for (seg, dq) in s.dq.iter_mut().enumerate() {
        let mut q = base_q;
        if use_segment {
            q = quantizer[seg] + if absolute { 0 } else { base_q };
        }
        dq[Y1][0] = i32::from(DC_TABLE[clip(q + delta[0], 127)]);
        dq[Y1][1] = i32::from(AC_TABLE[clip(q, 127)]);
        dq[Y2][0] = i32::from(DC_TABLE[clip(q + delta[1], 127)]) * 2;
        dq[Y2][1] = ((i32::from(AC_TABLE[clip(q + delta[2], 127)]) * 101581) >> 16).max(8);
        dq[UV][0] = i32::from(DC_TABLE[clip(q + delta[3], 117)]);
        dq[UV][1] = i32::from(AC_TABLE[clip(q + delta[4], 127)]);
    }
}

#[inline(always)]
fn large_value(br: &mut BoolDecoder, src: &mut Source, p: &[u8; 11]) -> i32 {
    if br.bit(src, p[3]) == 0 {
        if br.bit(src, p[4]) == 0 {
            return 2;
        }
        return 3 + br.bit(src, p[5]) as i32;
    }
    if br.bit(src, p[6]) == 0 {
        if br.bit(src, p[7]) == 0 {
            return 5 + br.bit(src, 159) as i32;
        }
        let v = 7 + 2 * br.bit(src, 165) as i32;
        return v + br.bit(src, 145) as i32;
    }
    let bit1 = br.bit(src, p[8]) as usize;
    let bit0 = br.bit(src, p[9 + bit1]) as usize;
    let cat = 2 * bit1 + bit0;
    let mut v = 0;
    for &prob in CAT3456[cat] {
        v += v + br.bit(src, prob) as i32;
    }
    v + 3 + (8 << cat)
}

// Returns one past the last coefficient read. Values are stored the way libwebp
// stores them, truncated to 16 bits after dequantisation.
#[inline(always)]
fn get_coeffs(br: &mut BoolDecoder, src: &mut Source, probs: &TypeProbs, ctx: usize, dq: &[i32; 2], mut n: usize, out: *mut i16) -> usize {
    // n stays below 17, and ctx below 3.
    let band = |n: usize| unsafe { probs.get_unchecked(usize::from(*BANDS.get_unchecked(n))) };
    let mut p = unsafe { band(n).get_unchecked(ctx) };
    while n < 16 {
        if br.bit(src, p[0]) == 0 {
            return n;
        }
        while br.bit(src, p[1]) == 0 {
            n += 1;
            p = &band(n)[0];
            if n == 16 {
                return 16;
            }
        }
        let next = band(n + 1);
        let v;
        if br.bit(src, p[2]) == 0 {
            v = 1;
            p = &next[1];
        } else {
            v = large_value(br, src, p);
            p = &next[2];
        }
        let value = br.signed(src, v).wrapping_mul(dq[usize::from(n > 0)]);
        unsafe { *out.add(usize::from(*ZIGZAG.get_unchecked(n))) = value as i16 };
        n += 1;
    }
    16
}

fn wht(input: &[i16; 16], out: &mut [i16]) {
    let i16s = |i: usize| i32::from(input[i]);
    let mut tmp = [0i32; 16];
    for i in 0..4 {
        let a0 = i16s(i) + i16s(12 + i);
        let a1 = i16s(4 + i) + i16s(8 + i);
        let a2 = i16s(4 + i) - i16s(8 + i);
        let a3 = i16s(i) - i16s(12 + i);
        tmp[i] = a0 + a1;
        tmp[8 + i] = a0 - a1;
        tmp[4 + i] = a3 + a2;
        tmp[12 + i] = a3 - a2;
    }
    for i in 0..4 {
        let dc = tmp[i * 4] + 3;
        let a0 = dc + tmp[3 + i * 4];
        let a1 = tmp[1 + i * 4] + tmp[2 + i * 4];
        let a2 = tmp[1 + i * 4] - tmp[2 + i * 4];
        let a3 = dc - tmp[3 + i * 4];
        out[i * 64] = ((a0 + a1) >> 3) as i16;
        out[16 + i * 64] = ((a3 + a2) >> 3) as i16;
        out[32 + i * 64] = ((a0 - a1) >> 3) as i16;
        out[48 + i * 64] = ((a3 - a2) >> 3) as i16;
    }
}

#[inline(always)]
fn block_kind(nz: usize, dc: i16) -> u8 {
    if nz > 1 { 2 } else { u8::from(dc != 0) }
}

#[inline(always)]
fn clip_8b(v: i32) -> u8 {
    if v & !0xff == 0 {
        v as u8
    } else if v < 0 {
        0
    } else {
        255
    }
}

#[inline(always)]
const fn mul1(a: i32) -> i32 {
    ((a * 20091) >> 16) + a
}

#[inline(always)]
const fn mul2(a: i32) -> i32 {
    (a * 35468) >> 16
}

unsafe fn idct_add(input: *const i16, mut dst: *mut u8) {
    let at = |i: usize| i32::from(unsafe { *input.add(i) });
    let mut tmp = [0i32; 16];
    for i in 0..4 {
        let a = at(i) + at(8 + i);
        let b = at(i) - at(8 + i);
        let c = mul2(at(4 + i)) - mul1(at(12 + i));
        let e = mul1(at(4 + i)) + mul2(at(12 + i));
        tmp[4 * i] = a + e;
        tmp[4 * i + 1] = b + c;
        tmp[4 * i + 2] = b - c;
        tmp[4 * i + 3] = a - e;
    }
    for i in 0..4 {
        let dc = tmp[i] + 4;
        let a = dc + tmp[8 + i];
        let b = dc - tmp[8 + i];
        let c = mul2(tmp[4 + i]) - mul1(tmp[12 + i]);
        let e = mul1(tmp[4 + i]) + mul2(tmp[12 + i]);
        unsafe {
            *dst = clip_8b(i32::from(*dst) + ((a + e) >> 3));
            *dst.add(1) = clip_8b(i32::from(*dst.add(1)) + ((b + c) >> 3));
            *dst.add(2) = clip_8b(i32::from(*dst.add(2)) + ((b - c) >> 3));
            *dst.add(3) = clip_8b(i32::from(*dst.add(3)) + ((a - e) >> 3));
            dst = dst.add(BPS);
        }
    }
}

unsafe fn dc_add(input: *const i16, dst: *mut u8) {
    let dc = i32::from(unsafe { *input }) + 4;
    for y in 0..4 {
        for x in 0..4 {
            unsafe {
                let p = dst.add(x + y * BPS);
                *p = clip_8b(i32::from(*p) + (dc >> 3));
            }
        }
    }
}

#[inline(always)]
unsafe fn add_block(kind: u8, input: *const i16, dst: *mut u8) {
    unsafe {
        if kind == 2 {
            idct_add(input, dst);
        } else if kind == 1 {
            dc_add(input, dst);
        }
    }
}

// Every pointer below stays inside the work block, whose margins hold the row
// above and the column to the left.
unsafe fn true_motion(mut dst: *mut u8, size: usize) {
    unsafe {
        let top = dst.offset(-IBPS);
        for _ in 0..size {
            let left = i32::from(*dst.offset(-1)) - i32::from(*top.offset(-1));
            for x in 0..size {
                *dst.add(x) = clip_8b(i32::from(*top.add(x)) + left);
            }
            dst = dst.add(BPS);
        }
    }
}

unsafe fn fill(dst: *mut u8, size: usize, v: u8) {
    for y in 0..size {
        unsafe { ptr::write_bytes(dst.add(y * BPS), v, size) };
    }
}

unsafe fn predict_block(dst: *mut u8, size: usize, mode: u8) {
    let log2 = if size == 16 { 4 } else { 3 };
    unsafe {
        let top = |i: usize| i32::from(*dst.add(i).offset(-IBPS));
        let left = |i: usize| i32::from(*dst.add(i * BPS).offset(-1));
        match mode {
            TM_PRED => true_motion(dst, size),
            V_PRED => {
                for y in 0..size {
                    ptr::copy_nonoverlapping(dst.offset(-IBPS), dst.add(y * BPS), size);
                }
            }
            H_PRED => {
                for y in 0..size {
                    ptr::write_bytes(dst.add(y * BPS), *dst.add(y * BPS).offset(-1), size);
                }
            }
            DC_PRED => {
                let dc = (0..size).fold(size as i32, |dc, i| dc + top(i) + left(i));
                fill(dst, size, (dc >> (log2 + 1)) as u8);
            }
            DC_NOTOP => {
                let dc = (0..size).fold(size as i32 >> 1, |dc, i| dc + left(i));
                fill(dst, size, (dc >> log2) as u8);
            }
            DC_NOLEFT => {
                let dc = (0..size).fold(size as i32 >> 1, |dc, i| dc + top(i));
                fill(dst, size, (dc >> log2) as u8);
            }
            _ => fill(dst, size, 0x80),
        }
    }
}

#[inline(always)]
const fn avg3(a: i32, b: i32, c: i32) -> u8 {
    ((a + 2 * b + c + 2) >> 2) as u8
}

#[inline(always)]
const fn avg2(a: i32, b: i32) -> u8 {
    ((a + b + 1) >> 1) as u8
}

unsafe fn predict4(dst: *mut u8, mode: u8) {
    unsafe {
        let top = dst.offset(-IBPS);
        let at = |p: *const u8, i: isize| i32::from(*p.offset(i));
        let (x, a, b, c, d) = (at(top, -1), at(top, 0), at(top, 1), at(top, 2), at(top, 3));
        let (e, f, g, h) = (at(top, 4), at(top, 5), at(top, 6), at(top, 7));
        let (i, j, k, l) = (at(dst, -1), at(dst, IBPS - 1), at(dst, 2 * IBPS - 1), at(dst, 3 * IBPS - 1));
        macro_rules! set {
            ($v:expr; $(($x:expr, $y:expr)),+) => {{
                let v = $v;
                $(*dst.add($x + $y * BPS) = v;)+
            }};
        }
        match mode {
            B_DC_PRED => {
                let dc = (0..4).fold(4, |dc, n| dc + at(top, n) + at(dst, n * IBPS - 1));
                fill(dst, 4, (dc >> 3) as u8);
            }
            B_TM_PRED => true_motion(dst, 4),
            B_VE_PRED => {
                let v = [avg3(x, a, b), avg3(a, b, c), avg3(b, c, d), avg3(c, d, e)];
                for y in 0..4 {
                    ptr::copy_nonoverlapping(v.as_ptr(), dst.add(y * BPS), 4);
                }
            }
            B_HE_PRED => {
                ptr::write_bytes(dst, avg3(x, i, j), 4);
                ptr::write_bytes(dst.add(BPS), avg3(i, j, k), 4);
                ptr::write_bytes(dst.add(2 * BPS), avg3(j, k, l), 4);
                ptr::write_bytes(dst.add(3 * BPS), avg3(k, l, l), 4);
            }
            B_RD_PRED => {
                set!(avg3(j, k, l); (0, 3));
                set!(avg3(i, j, k); (1, 3), (0, 2));
                set!(avg3(x, i, j); (2, 3), (1, 2), (0, 1));
                set!(avg3(a, x, i); (3, 3), (2, 2), (1, 1), (0, 0));
                set!(avg3(b, a, x); (3, 2), (2, 1), (1, 0));
                set!(avg3(c, b, a); (3, 1), (2, 0));
                set!(avg3(d, c, b); (3, 0));
            }
            B_LD_PRED => {
                set!(avg3(a, b, c); (0, 0));
                set!(avg3(b, c, d); (1, 0), (0, 1));
                set!(avg3(c, d, e); (2, 0), (1, 1), (0, 2));
                set!(avg3(d, e, f); (3, 0), (2, 1), (1, 2), (0, 3));
                set!(avg3(e, f, g); (3, 1), (2, 2), (1, 3));
                set!(avg3(f, g, h); (3, 2), (2, 3));
                set!(avg3(g, h, h); (3, 3));
            }
            B_VR_PRED => {
                set!(avg2(x, a); (0, 0), (1, 2));
                set!(avg2(a, b); (1, 0), (2, 2));
                set!(avg2(b, c); (2, 0), (3, 2));
                set!(avg2(c, d); (3, 0));
                set!(avg3(k, j, i); (0, 3));
                set!(avg3(j, i, x); (0, 2));
                set!(avg3(i, x, a); (0, 1), (1, 3));
                set!(avg3(x, a, b); (1, 1), (2, 3));
                set!(avg3(a, b, c); (2, 1), (3, 3));
                set!(avg3(b, c, d); (3, 1));
            }
            B_VL_PRED => {
                set!(avg2(a, b); (0, 0));
                set!(avg2(b, c); (1, 0), (0, 2));
                set!(avg2(c, d); (2, 0), (1, 2));
                set!(avg2(d, e); (3, 0), (2, 2));
                set!(avg3(a, b, c); (0, 1));
                set!(avg3(b, c, d); (1, 1), (0, 3));
                set!(avg3(c, d, e); (2, 1), (1, 3));
                set!(avg3(d, e, f); (3, 1), (2, 3));
                set!(avg3(e, f, g); (3, 2));
                set!(avg3(f, g, h); (3, 3));
            }
            B_HD_PRED => {
                set!(avg2(i, x); (0, 0), (2, 1));
                set!(avg2(j, i); (0, 1), (2, 2));
                set!(avg2(k, j); (0, 2), (2, 3));
                set!(avg2(l, k); (0, 3));
                set!(avg3(a, b, c); (3, 0));
                set!(avg3(x, a, b); (2, 0));
                set!(avg3(i, x, a); (1, 0), (3, 1));
                set!(avg3(j, i, x); (1, 1), (3, 2));
                set!(avg3(k, j, i); (1, 2), (3, 3));
                set!(avg3(l, k, j); (1, 3));
            }
            _ => {
                set!(avg2(i, j); (0, 0));
                set!(avg2(j, k); (2, 0), (0, 1));
                set!(avg2(k, l); (2, 1), (0, 2));
                set!(avg3(i, j, k); (1, 0));
                set!(avg3(j, k, l); (3, 0), (1, 1));
                set!(avg3(k, l, l); (3, 1), (1, 2));
                set!(l as u8; (3, 2), (2, 2), (0, 3), (1, 3), (2, 3), (3, 3));
            }
        }
    }
}

const fn edge_mode(mb_x: usize, mb_y: usize, mode: u8) -> u8 {
    if mode != DC_PRED {
        return mode;
    }
    if mb_x == 0 {
        return if mb_y == 0 { DC_NOTOPLEFT } else { DC_NOLEFT };
    }
    if mb_y == 0 { DC_NOTOP } else { DC_PRED }
}

#[inline(always)]
fn sclip1(v: i32) -> i32 {
    v.clamp(-128, 127)
}

#[inline(always)]
fn sclip2(v: i32) -> i32 {
    v.clamp(-16, 15)
}

// `p` is on an edge of a macroblock in the cache rows, with four rows above it.
#[inline(always)]
unsafe fn get(p: *mut u8, i: isize) -> i32 {
    i32::from(unsafe { *p.offset(i) })
}

#[inline(always)]
unsafe fn filter2(p: *mut u8, step: isize) {
    unsafe {
        let (p1, p0, q0, q1) = (get(p, -2 * step), get(p, -step), get(p, 0), get(p, step));
        let a = 3 * (q0 - p0) + sclip1(p1 - q1);
        let a1 = sclip2((a + 4) >> 3);
        let a2 = sclip2((a + 3) >> 3);
        *p.offset(-step) = clip_8b(p0 + a2);
        *p = clip_8b(q0 - a1);
    }
}

#[inline(always)]
unsafe fn filter4(p: *mut u8, step: isize) {
    unsafe {
        let (p1, p0, q0, q1) = (get(p, -2 * step), get(p, -step), get(p, 0), get(p, step));
        let a = 3 * (q0 - p0);
        let a1 = sclip2((a + 4) >> 3);
        let a2 = sclip2((a + 3) >> 3);
        let a3 = (a1 + 1) >> 1;
        *p.offset(-2 * step) = clip_8b(p1 + a3);
        *p.offset(-step) = clip_8b(p0 + a2);
        *p = clip_8b(q0 - a1);
        *p.offset(step) = clip_8b(q1 - a3);
    }
}

#[inline(always)]
unsafe fn filter6(p: *mut u8, step: isize) {
    unsafe {
        let (p2, p1, p0) = (get(p, -3 * step), get(p, -2 * step), get(p, -step));
        let (q0, q1, q2) = (get(p, 0), get(p, step), get(p, 2 * step));
        let a = sclip1(3 * (q0 - p0) + sclip1(p1 - q1));
        let a1 = (27 * a + 63) >> 7;
        let a2 = (18 * a + 63) >> 7;
        let a3 = (9 * a + 63) >> 7;
        *p.offset(-3 * step) = clip_8b(p2 + a3);
        *p.offset(-2 * step) = clip_8b(p1 + a2);
        *p.offset(-step) = clip_8b(p0 + a1);
        *p = clip_8b(q0 - a1);
        *p.offset(step) = clip_8b(q1 - a2);
        *p.offset(2 * step) = clip_8b(q2 - a3);
    }
}

#[inline(always)]
unsafe fn hev(p: *mut u8, step: isize, thresh: i32) -> bool {
    unsafe { (get(p, -2 * step) - get(p, -step)).abs() > thresh || (get(p, step) - get(p, 0)).abs() > thresh }
}

#[inline(always)]
unsafe fn needs_filter(p: *mut u8, step: isize, t: i32) -> bool {
    unsafe { 4 * (get(p, -step) - get(p, 0)).abs() + (get(p, -2 * step) - get(p, step)).abs() <= t }
}

#[inline(always)]
unsafe fn needs_filter2(p: *mut u8, step: isize, t: i32, it: i32) -> bool {
    unsafe {
        let (p3, p2, p1, p0) = (get(p, -4 * step), get(p, -3 * step), get(p, -2 * step), get(p, -step));
        let (q0, q1, q2, q3) = (get(p, 0), get(p, step), get(p, 2 * step), get(p, 3 * step));
        if 4 * (p0 - q0).abs() + (p1 - q1).abs() > t {
            return false;
        }
        (p3 - p2).abs() <= it
            && (p2 - p1).abs() <= it
            && (p1 - p0).abs() <= it
            && (q3 - q2).abs() <= it
            && (q2 - q1).abs() <= it
            && (q1 - q0).abs() <= it
    }
}

unsafe fn simple_edge(mut p: *mut u8, step: isize, along: isize, thresh: i32) {
    let t = 2 * thresh + 1;
    for _ in 0..16 {
        unsafe {
            if needs_filter(p, step, t) {
                filter2(p, step);
            }
            p = p.offset(along);
        }
    }
}

unsafe fn edge(mut p: *mut u8, step: isize, along: isize, size: usize, thresh: i32, ithresh: i32, hev_t: i32, mb_edge: bool) {
    let t = 2 * thresh + 1;
    for _ in 0..size {
        unsafe {
            if needs_filter2(p, step, t, ithresh) {
                if hev(p, step, hev_t) {
                    filter2(p, step);
                } else if mb_edge {
                    filter6(p, step);
                } else {
                    filter4(p, step);
                }
            }
            p = p.offset(along);
        }
    }
}

#[inline(always)]
fn clip_rgb(v: i32) -> u8 {
    if v & !16383 == 0 {
        (v >> 6) as u8
    } else if v < 0 {
        0
    } else {
        255
    }
}

#[inline(always)]
unsafe fn yuv_to_rgb(y: i32, u: i32, v: i32, rgb: *mut u8) {
    let luma = (y * 19077) >> 8;
    unsafe {
        *rgb = clip_rgb(luma + ((v * 26149) >> 8) - 14234);
        *rgb.add(1) = clip_rgb(luma - ((u * 6419) >> 8) - ((v * 13320) >> 8) + 8708);
        *rgb.add(2) = clip_rgb(luma + ((u * 33050) >> 8) - 17685);
    }
}

unsafe fn upsample(y: *const u8, nu: *const u8, nv: *const u8, fu: *const u8, fv: *const u8, dst: *mut u8, len: usize) {
    unsafe {
        let at = |p: *const u8, i: usize| i32::from(*p.add(i));
        let (mut tl_u, mut tl_v, mut l_u, mut l_v) = (at(nu, 0), at(nv, 0), at(fu, 0), at(fv, 0));
        yuv_to_rgb(at(y, 0), (3 * tl_u + l_u + 2) >> 2, (3 * tl_v + l_v + 2) >> 2, dst);
        for x in 1..=(len - 1) >> 1 {
            let (t_u, t_v, u, v) = (at(nu, x), at(nv, x), at(fu, x), at(fv, x));
            let avg_u = tl_u + t_u + l_u + u + 8;
            let avg_v = tl_v + t_v + l_v + v + 8;
            let d12_u = (avg_u + 2 * (t_u + l_u)) >> 3;
            let d12_v = (avg_v + 2 * (t_v + l_v)) >> 3;
            let d03_u = (avg_u + 2 * (tl_u + u)) >> 3;
            let d03_v = (avg_v + 2 * (tl_v + v)) >> 3;
            yuv_to_rgb(at(y, 2 * x - 1), (d12_u + tl_u) >> 1, (d12_v + tl_v) >> 1, dst.add((2 * x - 1) * 3));
            yuv_to_rgb(at(y, 2 * x), (d03_u + t_u) >> 1, (d03_v + t_v) >> 1, dst.add(2 * x * 3));
            (tl_u, tl_v, l_u, l_v) = (t_u, t_v, u, v);
        }
        if len & 1 == 0 {
            yuv_to_rgb(at(y, len - 1), (3 * tl_u + l_u + 2) >> 2, (3 * tl_v + l_v + 2) >> 2, dst.add((len - 1) * 3));
        }
    }
}

impl Vp8 {
    pub(crate) fn open(src: &mut Source, pos: usize, len: usize, caps: Option<Caps>) -> Result<Self> {
        let mut hdr = [0; 10];
        if len < 10 {
            return Err(Error::Decode);
        }
        if src.read(pos, &mut hdr) != 10 {
            return Err(Error::Truncated);
        }
        let bits = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], 0]);
        if bits & 1 != 0 || (bits >> 1) & 7 > 3 {
            return Err(Error::Decode);
        }
        if (bits >> 4) & 1 == 0 {
            return Err(Error::Unsupported);
        }
        if hdr[3..6] != [0x9d, 0x01, 0x2a] {
            return Err(Error::Decode);
        }
        let width = usize::from(u16::from_le_bytes([hdr[6], hdr[7]]) & 0x3fff);
        let height = usize::from(u16::from_le_bytes([hdr[8], hdr[9]]) & 0x3fff);
        if width == 0 || height == 0 {
            return Err(Error::Decode);
        }
        let part0_len = (bits >> 5) as usize;
        if part0_len > len - 10 {
            return Err(Error::Decode);
        }

        let mb_w = (width + 15) >> 4;
        let mb_h = (height + 15) >> 4;
        let (y_stride, uv_stride) = (mb_w * 16, mb_w * 8);
        let mut br = BoolDecoder::new(Cursor::new(pos + 10, part0_len, PART0_WINDOW, caps)?);
        br.start(src);
        let mut d = Self {
            width: width as u32,
            height: height as u32,
            mb_w,
            mb_h,
            br,
            parts: [const { None }; 8],
            num_parts: 0,
            update_map: false,
            filter_type: 0,
            use_skip: false,
            skip_p: 0,
            s: state()?,
            intra_t: buffer(4 * mb_w, caps)?,
            intra_l: [B_DC_PRED; 4],
            nz_t: buffer(mb_w, caps)?,
            nz_dc_t: buffer(mb_w, caps)?,
            nz_l: 0,
            nz_dc_l: 0,
            top: buffer(32 * mb_w, caps)?,
            finfo: buffer(mb_w, caps)?,
            cache_y: buffer((CACHE_ABOVE + 16) * y_stride, caps)?,
            cache_u: buffer((CACHE_ABOVE + 8) * uv_stride, caps)?,
            cache_v: buffer((CACHE_ABOVE + 8) * uv_stride, caps)?,
            y_stride,
            uv_stride,
            mb_y: 0,
            y_final: 0,
            uv_final: 0,
            out_y: 0,
        };
        d.parse_header(src, pos + 10 + part0_len, pos + len, caps)?;
        for part in d.parts[..d.num_parts].iter_mut().flatten() {
            part.start(src);
        }
        Ok(d)
    }

    pub(crate) const fn width(&self) -> u32 {
        self.width
    }

    pub(crate) const fn height(&self) -> u32 {
        self.height
    }

    fn parse_header(&mut self, src: &mut Source, parts_pos: usize, parts_end: usize, caps: Option<Caps>) -> Result<()> {
        let br = &mut self.br;
        br.value(src, 2);

        let (mut quantizer, mut strength) = ([0; 4], [0; 4]);
        let use_segment = br.flag(src);
        let mut absolute = true;
        self.s.seg_probs = [255; 3];
        if use_segment {
            self.update_map = br.flag(src);
            if br.flag(src) {
                absolute = br.flag(src);
                for q in &mut quantizer {
                    *q = if br.flag(src) { i32::from(br.signed_value(src, 7) as i8) } else { 0 };
                }
                for s in &mut strength {
                    *s = if br.flag(src) { i32::from(br.signed_value(src, 6) as i8) } else { 0 };
                }
            }
            if self.update_map {
                for p in &mut self.s.seg_probs {
                    *p = if br.flag(src) { br.value(src, 8) as u8 } else { 255 };
                }
            }
        }

        let simple = br.flag(src);
        let level = br.value(src, 6) as i32;
        let sharpness = br.value(src, 3) as i32;
        let use_lf_delta = br.flag(src);
        let (mut ref_delta, mut mode_delta) = ([0; 4], [0; 4]);
        if use_lf_delta && br.flag(src) {
            for d in &mut ref_delta {
                if br.flag(src) {
                    *d = br.signed_value(src, 6);
                }
            }
            for d in &mut mode_delta {
                if br.flag(src) {
                    *d = br.signed_value(src, 6);
                }
            }
        }
        self.filter_type = if level == 0 {
            0
        } else if simple {
            1
        } else {
            2
        };
        for (seg, strengths) in self.s.fstrengths.iter_mut().enumerate() {
            let mut base = level;
            if use_segment {
                base = strength[seg] + if absolute { 0 } else { level };
            }
            for (i4x4, info) in strengths.iter_mut().enumerate() {
                let mut lv = base;
                if use_lf_delta {
                    lv += ref_delta[0];
                    if i4x4 != 0 {
                        lv += mode_delta[0];
                    }
                }
                let lv = lv.clamp(0, 63);
                info.inner = i4x4 as u8;
                if lv == 0 {
                    info.limit = 0;
                    continue;
                }
                let mut ilevel = lv;
                if sharpness > 0 {
                    ilevel >>= if sharpness > 4 { 2 } else { 1 };
                    ilevel = ilevel.min(9 - sharpness);
                }
                let ilevel = ilevel.max(1);
                info.ilevel = ilevel as u8;
                info.limit = (2 * lv + ilevel) as u8;
                info.hev = if lv >= 40 {
                    2
                } else if lv >= 15 {
                    1
                } else {
                    0
                };
            }
        }

        self.num_parts = 1 << br.value(src, 2);
        let mut sizes = [0; 21];
        let table = 3 * (self.num_parts - 1);
        if parts_end - parts_pos < table {
            return Err(Error::Decode);
        }
        if src.read(parts_pos, &mut sizes[..table]) != table {
            return Err(Error::Truncated);
        }
        let mut pos = parts_pos + table;
        for p in 0..self.num_parts {
            let mut len = parts_end - pos;
            if p < self.num_parts - 1 {
                let want = u32::from_le_bytes([sizes[3 * p], sizes[3 * p + 1], sizes[3 * p + 2], 0]) as usize;
                len = len.min(want);
            } else if len == 0 {
                return Err(Error::Decode);
            }
            self.parts[p] = Some(BoolDecoder::new(Cursor::new(pos, len, TOKEN_WINDOW, caps)?));
            pos += len;
        }

        parse_quant(&mut self.s, br, src, &quantizer, use_segment, absolute);
        br.flag(src);

        for t in 0..4 {
            for b in 0..8 {
                for c in 0..3 {
                    for p in 0..11 {
                        let i = ((t * 8 + b) * 3 + c) * 11 + p;
                        self.s.probs[t][b][c][p] =
                            if br.bit(src, COEFF_UPDATE_PROBS[i]) != 0 { br.value(src, 8) as u8 } else { COEFF_PROBS[i] };
                    }
                }
            }
        }
        self.use_skip = br.flag(src);
        if self.use_skip {
            self.skip_p = br.value(src, 8) as u8;
        }
        if br.eof { Err(src.eof_error()) } else { Ok(()) }
    }

    fn parse_modes(&mut self, src: &mut Source, mb_x: usize, mb: &mut Macroblock) {
        let br = &mut self.br;
        let top = &mut self.intra_t[4 * mb_x..4 * mb_x + 4];
        let left = &mut self.intra_l;
        let seg = &self.s.seg_probs;
        mb.segment = if !self.update_map {
            0
        } else if br.bit(src, seg[0]) == 0 {
            br.bit(src, seg[1]) as u8
        } else {
            2 + br.bit(src, seg[2]) as u8
        };
        mb.skip = self.use_skip && br.bit(src, self.skip_p) != 0;
        mb.is_i4x4 = br.bit(src, 145) == 0;
        if !mb.is_i4x4 {
            let ymode = if br.bit(src, 156) != 0 {
                if br.bit(src, 128) != 0 { TM_PRED } else { H_PRED }
            } else if br.bit(src, 163) != 0 {
                V_PRED
            } else {
                DC_PRED
            };
            mb.imodes[0] = ymode;
            top.fill(ymode);
            left.fill(ymode);
        } else {
            for y in 0..4 {
                let mut ymode = left[y];
                for x in 0..4 {
                    let at = (usize::from(top[x]) * 10 + usize::from(ymode)) * 9;
                    let p: &[u8; 9] = BMODE_PROBS[at..at + 9].try_into().unwrap();
                    ymode = if br.bit(src, p[0]) == 0 {
                        B_DC_PRED
                    } else if br.bit(src, p[1]) == 0 {
                        B_TM_PRED
                    } else if br.bit(src, p[2]) == 0 {
                        B_VE_PRED
                    } else if br.bit(src, p[3]) == 0 {
                        if br.bit(src, p[4]) == 0 {
                            B_HE_PRED
                        } else if br.bit(src, p[5]) == 0 {
                            B_RD_PRED
                        } else {
                            B_VR_PRED
                        }
                    } else if br.bit(src, p[6]) == 0 {
                        B_LD_PRED
                    } else if br.bit(src, p[7]) == 0 {
                        B_VL_PRED
                    } else if br.bit(src, p[8]) == 0 {
                        B_HD_PRED
                    } else {
                        B_HU_PRED
                    };
                    top[x] = ymode;
                    mb.imodes[y * 4 + x] = ymode;
                }
                left[y] = ymode;
            }
        }
        mb.uvmode = if br.bit(src, 142) == 0 {
            DC_PRED
        } else if br.bit(src, 114) == 0 {
            V_PRED
        } else if br.bit(src, 183) != 0 {
            TM_PRED
        } else {
            H_PRED
        };
    }

    /// `true` when the macroblock has no coefficient at all.
    fn parse_residuals(&mut self, src: &mut Source, mb_x: usize, mb: &Macroblock, br: &mut BoolDecoder) -> bool {
        let s = &mut *self.s;
        let dq = &s.dq[usize::from(mb.segment)];
        let probs = &s.probs;
        s.coeffs.fill(0);
        let dst = s.coeffs.as_mut_ptr();
        let mut any = false;

        let (first, ac_probs) = if !mb.is_i4x4 {
            let mut dc = [0i16; 16];
            let ctx = usize::from(self.nz_dc_t[mb_x] + self.nz_dc_l);
            let nz = get_coeffs(br, src, &probs[1], ctx, &dq[Y2], 0, dc.as_mut_ptr());
            let flag = u8::from(nz > 0);
            self.nz_dc_t[mb_x] = flag;
            self.nz_dc_l = flag;
            if nz > 1 {
                wht(&dc, &mut s.coeffs[..]);
            } else {
                let dc0 = (i32::from(dc[0]) + 3) >> 3;
                for i in (0..256).step_by(16) {
                    s.coeffs[i] = dc0 as i16;
                }
            }
            (1, &probs[0])
        } else {
            (0, &probs[3])
        };

        let mut tnz = u32::from(self.nz_t[mb_x]);
        let mut lnz = u32::from(self.nz_l);
        for y in 0..4 {
            let mut l = (lnz >> y) & 1;
            for x in 0..4 {
                let ctx = (l + ((tnz >> x) & 1)) as usize;
                let n = y * 4 + x;
                let block = unsafe { dst.add(n * 16) };
                let nz = get_coeffs(br, src, ac_probs, ctx, &dq[Y1], first, block);
                l = u32::from(nz > first);
                tnz = (tnz & !(1 << x)) | (l << x);
                let kind = block_kind(nz, unsafe { *block });
                s.kinds[n] = kind;
                any |= kind != 0;
            }
            lnz = (lnz & !(1 << y)) | (l << y);
        }
        for ch in 0..2 {
            let shift = 4 + 2 * ch;
            for y in 0..2 {
                let mut l = (lnz >> (shift + y)) & 1;
                for x in 0..2 {
                    let ctx = (l + ((tnz >> (shift + x)) & 1)) as usize;
                    let b = (16 + ch * 4 + y * 2 + x) as usize;
                    let block = unsafe { dst.add(b * 16) };
                    let nz = get_coeffs(br, src, &probs[2], ctx, &dq[UV], 0, block);
                    l = u32::from(nz > 0);
                    tnz = (tnz & !(1 << (shift + x))) | (l << (shift + x));
                    let kind = block_kind(nz, unsafe { *block });
                    s.kinds[b] = kind;
                    any |= kind != 0;
                }
                lnz = (lnz & !(1 << (shift + y))) | (l << (shift + y));
            }
        }
        self.nz_t[mb_x] = tnz as u8;
        self.nz_l = lnz as u8;
        !any
    }

    fn reconstruct(&mut self, mb_x: usize, mb: &Macroblock) {
        let mb_y = self.mb_y;
        let s = &mut *self.s;
        let work = s.work.as_mut_ptr();
        let coeffs = s.coeffs.as_ptr();
        let kinds = &s.kinds;
        unsafe {
            let y_dst = work.add(Y_OFF);
            let u_dst = work.add(U_OFF);
            let v_dst = work.add(V_OFF);
            let top = self.top.as_mut_ptr().add(mb_x * 32);
            let row = |p: *mut u8, j: isize, x: isize| p.offset(j * IBPS + x);

            if mb_x == 0 {
                for j in 0..16 {
                    *row(y_dst, j, -1) = 129;
                }
                for j in 0..8 {
                    *row(u_dst, j, -1) = 129;
                    *row(v_dst, j, -1) = 129;
                }
                if mb_y > 0 {
                    *row(y_dst, -1, -1) = 129;
                    *row(u_dst, -1, -1) = 129;
                    *row(v_dst, -1, -1) = 129;
                } else {
                    ptr::write_bytes(row(y_dst, -1, -1), 127, 16 + 4 + 1);
                    ptr::write_bytes(row(u_dst, -1, -1), 127, 8 + 1);
                    ptr::write_bytes(row(v_dst, -1, -1), 127, 8 + 1);
                }
            } else {
                for j in -1..16 {
                    ptr::copy_nonoverlapping(row(y_dst, j, 12), row(y_dst, j, -4), 4);
                }
                for j in -1..8 {
                    ptr::copy_nonoverlapping(row(u_dst, j, 4), row(u_dst, j, -4), 4);
                    ptr::copy_nonoverlapping(row(v_dst, j, 4), row(v_dst, j, -4), 4);
                }
            }
            if mb_y > 0 {
                ptr::copy_nonoverlapping(top, row(y_dst, -1, 0), 16);
                ptr::copy_nonoverlapping(top.add(16), row(u_dst, -1, 0), 8);
                ptr::copy_nonoverlapping(top.add(24), row(v_dst, -1, 0), 8);
            }

            if mb.is_i4x4 {
                let top_right = row(y_dst, -1, 16);
                if mb_y > 0 {
                    if mb_x >= self.mb_w - 1 {
                        ptr::write_bytes(top_right, *top.add(15), 4);
                    } else {
                        ptr::copy_nonoverlapping(top.add(32), top_right, 4);
                    }
                }
                for k in 1..4 {
                    ptr::copy_nonoverlapping(top_right, top_right.add(4 * k * BPS), 4);
                }
                for n in 0..16 {
                    let dst = y_dst.add((n >> 2) * 4 * BPS + (n & 3) * 4);
                    predict4(dst, mb.imodes[n]);
                    add_block(kinds[n], coeffs.add(n * 16), dst);
                }
            } else {
                predict_block(y_dst, 16, edge_mode(mb_x, mb_y, mb.imodes[0]));
                for n in 0..16 {
                    add_block(kinds[n], coeffs.add(n * 16), y_dst.add((n >> 2) * 4 * BPS + (n & 3) * 4));
                }
            }
            let uvmode = edge_mode(mb_x, mb_y, mb.uvmode);
            predict_block(u_dst, 8, uvmode);
            predict_block(v_dst, 8, uvmode);
            for n in 0..4 {
                let off = (n >> 1) * 4 * BPS + (n & 1) * 4;
                add_block(kinds[16 + n], coeffs.add((16 + n) * 16), u_dst.add(off));
                add_block(kinds[20 + n], coeffs.add((20 + n) * 16), v_dst.add(off));
            }

            ptr::copy_nonoverlapping(y_dst.add(15 * BPS), top, 16);
            ptr::copy_nonoverlapping(u_dst.add(7 * BPS), top.add(16), 8);
            ptr::copy_nonoverlapping(v_dst.add(7 * BPS), top.add(24), 8);

            let (ys, uvs) = (self.y_stride, self.uv_stride);
            let y_out = self.cache_y.as_mut_ptr().add(CACHE_ABOVE * ys + mb_x * 16);
            let u_out = self.cache_u.as_mut_ptr().add(CACHE_ABOVE * uvs + mb_x * 8);
            let v_out = self.cache_v.as_mut_ptr().add(CACHE_ABOVE * uvs + mb_x * 8);
            for j in 0..16 {
                ptr::copy_nonoverlapping(y_dst.add(j * BPS), y_out.add(j * ys), 16);
            }
            for j in 0..8 {
                ptr::copy_nonoverlapping(u_dst.add(j * BPS), u_out.add(j * uvs), 8);
                ptr::copy_nonoverlapping(v_dst.add(j * BPS), v_out.add(j * uvs), 8);
            }
        }
    }

    fn filter_mb(&mut self, mb_x: usize) {
        let f = self.finfo[mb_x];
        let limit = i32::from(f.limit);
        if limit == 0 {
            return;
        }
        let (ys, uvs) = (self.y_stride, self.uv_stride);
        let (ys_i, uvs_i) = (ys as isize, uvs as isize);
        let inner = f.inner != 0;
        let top_edge = self.mb_y > 0;
        unsafe {
            let y = self.cache_y.as_mut_ptr().add(CACHE_ABOVE * ys + mb_x * 16);
            if self.filter_type == 1 {
                if mb_x > 0 {
                    simple_edge(y, 1, ys_i, limit + 4);
                }
                if inner {
                    for k in [4, 8, 12] {
                        simple_edge(y.add(k), 1, ys_i, limit);
                    }
                }
                if top_edge {
                    simple_edge(y, ys_i, 1, limit + 4);
                }
                if inner {
                    for k in [4, 8, 12] {
                        simple_edge(y.add(k * ys), ys_i, 1, limit);
                    }
                }
                return;
            }
            let u = self.cache_u.as_mut_ptr().add(CACHE_ABOVE * uvs + mb_x * 8);
            let v = self.cache_v.as_mut_ptr().add(CACHE_ABOVE * uvs + mb_x * 8);
            let (il, ht) = (i32::from(f.ilevel), i32::from(f.hev));
            if mb_x > 0 {
                edge(y, 1, ys_i, 16, limit + 4, il, ht, true);
                edge(u, 1, uvs_i, 8, limit + 4, il, ht, true);
                edge(v, 1, uvs_i, 8, limit + 4, il, ht, true);
            }
            if inner {
                for k in [4, 8, 12] {
                    edge(y.add(k), 1, ys_i, 16, limit, il, ht, false);
                }
                edge(u.add(4), 1, uvs_i, 8, limit, il, ht, false);
                edge(v.add(4), 1, uvs_i, 8, limit, il, ht, false);
            }
            if top_edge {
                edge(y, ys_i, 1, 16, limit + 4, il, ht, true);
                edge(u, uvs_i, 1, 8, limit + 4, il, ht, true);
                edge(v, uvs_i, 1, 8, limit + 4, il, ht, true);
            }
            if inner {
                for k in [4, 8, 12] {
                    edge(y.add(k * ys), ys_i, 1, 16, limit, il, ht, false);
                }
                edge(u.add(4 * uvs), uvs_i, 1, 8, limit, il, ht, false);
                edge(v.add(4 * uvs), uvs_i, 1, 8, limit, il, ht, false);
            }
        }
    }

    fn decode_mb_row(&mut self, src: &mut Source) -> Result<()> {
        if self.mb_y > 0 {
            let (ys, uvs) = (self.y_stride, self.uv_stride);
            self.cache_y.copy_within(16 * ys..(16 + CACHE_ABOVE) * ys, 0);
            self.cache_u.copy_within(8 * uvs..(8 + CACHE_ABOVE) * uvs, 0);
            self.cache_v.copy_within(8 * uvs..(8 + CACHE_ABOVE) * uvs, 0);
        }
        let part = self.mb_y & (self.num_parts - 1);
        let mut tokens = self.parts[part].take().unwrap();
        self.nz_l = 0;
        self.nz_dc_l = 0;
        self.intra_l = [B_DC_PRED; 4];
        let mut result = Ok(());
        for mb_x in 0..self.mb_w {
            let mut mb = Macroblock::default();
            self.parse_modes(src, mb_x, &mut mb);
            let mut skip = mb.skip;
            if !skip {
                skip = self.parse_residuals(src, mb_x, &mb, &mut tokens);
            } else {
                self.nz_t[mb_x] = 0;
                self.nz_l = 0;
                if !mb.is_i4x4 {
                    self.nz_dc_t[mb_x] = 0;
                    self.nz_dc_l = 0;
                }
                self.s.kinds = [0; 24];
            }
            if tokens.eof {
                result = Err(src.eof_error());
                break;
            }
            let mut info = self.s.fstrengths[usize::from(mb.segment)][usize::from(mb.is_i4x4)];
            info.inner |= u8::from(!skip);
            self.finfo[mb_x] = info;
            self.reconstruct(mb_x, &mb);
        }
        self.parts[part] = Some(tokens);
        result?;
        if self.br.eof {
            return Err(src.eof_error());
        }
        if self.filter_type != 0 {
            for mb_x in 0..self.mb_w {
                self.filter_mb(mb_x);
            }
        }

        self.mb_y += 1;
        if self.mb_y == self.mb_h {
            self.y_final = self.height as usize;
            self.uv_final = (self.height as usize + 1) >> 1;
        } else {
            let y_delay = match self.filter_type {
                2 => 3,
                1 => 1,
                _ => 0,
            };
            self.y_final = self.mb_y * 16 - y_delay;
            self.uv_final = self.mb_y * 8 - if self.filter_type == 2 { 3 } else { 0 };
        }
        Ok(())
    }

    fn chroma_needed(&self, y: usize) -> usize {
        if y == 0 {
            return 0;
        }
        let far = if y & 1 != 0 { (y + 1) >> 1 } else { (y >> 1) - 1 };
        let near = if y & 1 != 0 { (y - 1) >> 1 } else { y >> 1 };
        if far < (self.height as usize + 1) >> 1 && far > near { far } else { near }
    }

    pub(crate) fn next_row(&mut self, src: &mut Source, rgb: &mut [u8]) -> Result<()> {
        let y = self.out_y;
        let (height, width) = (self.height as usize, self.width as usize);
        if rgb.len() < width * 3 {
            return Err(Error::InvalidArgument);
        }
        while y >= self.y_final || self.chroma_needed(y) >= self.uv_final {
            if self.mb_y >= self.mb_h {
                return Err(Error::InvalidState);
            }
            self.decode_mb_row(src)?;
        }
        let base_y = (self.mb_y - 1) * 16;
        let base_uv = (self.mb_y - 1) * 8;
        let (near, far);
        if y == 0 {
            (near, far) = (0, 0);
        } else if y & 1 != 0 {
            near = (y - 1) >> 1;
            far = if (y + 1) >> 1 < (height + 1) >> 1 { (y + 1) >> 1 } else { near };
        } else {
            near = y >> 1;
            far = near - 1;
        }
        let uvs = self.uv_stride;
        let n_off = (near + CACHE_ABOVE - base_uv) * uvs;
        let f_off = (far + CACHE_ABOVE - base_uv) * uvs;
        let y_off = (y + CACHE_ABOVE - base_y) * self.y_stride;
        unsafe {
            upsample(
                self.cache_y[y_off..].as_ptr(),
                self.cache_u[n_off..].as_ptr(),
                self.cache_v[n_off..].as_ptr(),
                self.cache_u[f_off..].as_ptr(),
                self.cache_v[f_off..].as_ptr(),
                rgb.as_mut_ptr(),
                width,
            );
        }
        self.out_y += 1;
        Ok(())
    }
}
