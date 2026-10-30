/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;
use core::ptr;

use devkit::Zeroable;
use idf::heap::Caps;

use super::{Cursor, Source};
use crate::mem::{Buffer, buffer};
use crate::{Error, Result};

const WINDOW: usize = 32768;
const ROOT_BITS: usize = 8;
const MAX_CODE_LEN: usize = 15;
const NUM_LITERAL: usize = 256;
const NUM_LENGTH: usize = 24;
const NUM_DIST: usize = 40;
const MAX_CACHE_BITS: u32 = 11;
const MAX_COPY: usize = 4096;
const MAX_FAR_DIST: usize = 1048576 - 120;
const ARENA_BLOCK: usize = 65536 / core::mem::size_of::<HCode>();

const PREDICTOR: u32 = 0;
const CROSS_COLOR: u32 = 1;
const SUBTRACT_GREEN: u32 = 2;
const COLOR_INDEXING: u32 = 3;

const GREEN: usize = 0;
const RED: usize = 1;
const BLUE: usize = 2;
const ALPHA: usize = 3;
const DIST: usize = 4;

const CODE_LENGTH_ORDER: [u8; 19] = [17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

const CODE_TO_PLANE: [u8; 120] = [
    0x18, 0x07, 0x17, 0x19, 0x28, 0x06, 0x27, 0x29, 0x16, 0x1a,
    0x26, 0x2a, 0x38, 0x05, 0x37, 0x39, 0x15, 0x1b, 0x36, 0x3a,
    0x25, 0x2b, 0x48, 0x04, 0x47, 0x49, 0x14, 0x1c, 0x35, 0x3b,
    0x46, 0x4a, 0x24, 0x2c, 0x58, 0x45, 0x4b, 0x34, 0x3c, 0x03,
    0x57, 0x59, 0x13, 0x1d, 0x56, 0x5a, 0x23, 0x2d, 0x44, 0x4c,
    0x55, 0x5b, 0x33, 0x3d, 0x68, 0x02, 0x67, 0x69, 0x12, 0x1e,
    0x66, 0x6a, 0x22, 0x2e, 0x54, 0x5c, 0x43, 0x4d, 0x65, 0x6b,
    0x32, 0x3e, 0x78, 0x01, 0x77, 0x79, 0x53, 0x5d, 0x11, 0x1f,
    0x64, 0x6c, 0x42, 0x4e, 0x76, 0x7a, 0x21, 0x2f, 0x75, 0x7b,
    0x31, 0x3f, 0x63, 0x6d, 0x52, 0x5e, 0x00, 0x74, 0x7c, 0x41,
    0x4f, 0x10, 0x20, 0x62, 0x6e, 0x30, 0x73, 0x7d, 0x51, 0x5f,
    0x40, 0x72, 0x7e, 0x61, 0x6f, 0x50, 0x71, 0x7f, 0x60, 0x70,
];

/// Past the end it yields zeros and marks the stream overrun.
struct Bits {
    cursor: Cursor,
    pos: usize,
    len: usize,
    val: u64,
    nbits: u32,
    overrun: bool,
}

impl Bits {
    fn fill(&mut self, src: &mut Source) {
        if self.nbits <= 32 && self.pos + 4 <= self.len {
            let word = unsafe { self.cursor.buf.as_ptr().add(self.pos).cast::<[u8; 4]>().read() };
            self.val |= u64::from(u32::from_le_bytes(word)) << self.nbits;
            self.pos += 4;
            self.nbits += 32;
            return;
        }
        while self.nbits <= 56 {
            if self.pos == self.len {
                self.len = self.cursor.refill(src);
                self.pos = 0;
                if self.len == 0 {
                    return;
                }
            }
            self.val |= u64::from(unsafe { *self.cursor.buf.get_unchecked(self.pos) }) << self.nbits;
            self.pos += 1;
            self.nbits += 8;
        }
    }

    #[inline(always)]
    fn skip(&mut self, n: u32) {
        if n > self.nbits {
            self.overrun = true;
            self.val = 0;
            self.nbits = 0;
            return;
        }
        self.val >>= n;
        self.nbits -= n;
    }

    #[inline(always)]
    fn bits(&mut self, src: &mut Source, n: u32) -> u32 {
        if self.nbits < n {
            self.fill(src);
        }
        let v = (self.val & ((1u64 << n) - 1)) as u32;
        self.skip(n);
        v
    }

    fn stream_error(&self, src: &Source) -> Error {
        if self.overrun { src.eof_error() } else { Error::Decode }
    }
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
struct HCode {
    bits: u8,
    value: u16,
}

unsafe impl Zeroable for HCode {}

struct Block {
    data: Buffer<HCode>,
    used: usize,
}

/// Prefix-code tables, which group pointers refer to: blocks never move.
struct Arena {
    blocks: Vec<Block>,
    caps: Option<Caps>,
}

impl Arena {
    fn alloc(&mut self, count: usize) -> Option<*mut HCode> {
        if self.blocks.last().is_none_or(|b| b.data.len() - b.used < count) {
            let data = buffer(count.max(ARENA_BLOCK), self.caps).ok()?;
            self.blocks.try_reserve(1).ok()?;
            self.blocks.push(Block { data, used: 0 });
        }
        let b = self.blocks.last_mut()?;
        let p = unsafe { b.data.as_mut_ptr().add(b.used) };
        b.used += count;
        Some(p)
    }

    fn mark(&self) -> (usize, usize) {
        (self.blocks.len(), self.blocks.last().map_or(0, |b| b.used))
    }

    /// Takes back what came after `mark`, unless a new block began since.
    fn rewind(&mut self, (blocks, used): (usize, usize)) {
        if blocks != 0 && self.blocks.len() == blocks {
            self.blocks[blocks - 1].used = used;
        }
    }
}

fn reverse_bits(code: u32, len: usize) -> u32 {
    code.reverse_bits() >> (32 - len)
}

fn build_table(a: &mut Arena, lengths: &[u8]) -> Option<*const HCode> {
    let n = lengths.len();
    let root = 1usize << ROOT_BITS;
    let mut count = [0usize; MAX_CODE_LEN + 1];
    let mut single = 0;
    for (s, &len) in lengths.iter().enumerate() {
        count[usize::from(len)] += 1;
        if len != 0 {
            single = s;
        }
    }
    if count[0] == n {
        return None;
    }
    if count[0] == n - 1 {
        let t = a.alloc(root)?;
        for i in 0..root {
            unsafe { *t.add(i) = HCode { bits: 0, value: single as u16 } };
        }
        return Some(t);
    }
    let kraft: u32 = (1..=MAX_CODE_LEN).map(|len| (count[len] as u32) << (MAX_CODE_LEN - len)).sum();
    if kraft != 1 << MAX_CODE_LEN {
        return None;
    }

    let mut next = [0u32; MAX_CODE_LEN + 2];
    for len in 1..=MAX_CODE_LEN {
        next[len + 1] = (next[len] + count[len] as u32) << 1;
    }

    let mut sub_bits = [0u8; 1 << ROOT_BITS];
    let mut code = next;
    for &len in lengths {
        let len = usize::from(len);
        if len <= ROOT_BITS {
            continue;
        }
        let rev = reverse_bits(code[len], len);
        code[len] += 1;
        let sb = &mut sub_bits[rev as usize & (root - 1)];
        *sb = (*sb).max((len - ROOT_BITS) as u8);
    }
    let total = root + sub_bits.iter().filter(|&&b| b != 0).map(|&b| 1usize << b).sum::<usize>();

    let t = a.alloc(total)?;
    let table = unsafe { core::slice::from_raw_parts_mut(t, total) };
    let mut sub = root;
    for (i, &b) in sub_bits.iter().enumerate() {
        if b != 0 {
            table[i] = HCode { bits: ROOT_BITS as u8 + b, value: sub as u16 };
            sub += 1 << b;
        }
    }
    code = next;
    for (s, &len) in lengths.iter().enumerate() {
        let len = usize::from(len);
        if len == 0 {
            continue;
        }
        let rev = reverse_bits(code[len], len) as usize;
        code[len] += 1;
        if len <= ROOT_BITS {
            for i in (rev..root).step_by(1 << len) {
                table[i] = HCode { bits: len as u8, value: s as u16 };
            }
        } else {
            let link = table[rev & (root - 1)];
            let bits = usize::from(link.bits) - ROOT_BITS;
            let sub = usize::from(link.value);
            for i in ((rev >> ROOT_BITS)..1 << bits).step_by(1 << (len - ROOT_BITS)) {
                table[sub + i] = HCode { bits: (len - ROOT_BITS) as u8, value: s as u16 };
            }
        }
    }
    Some(t)
}

#[inline(always)]
fn read_symbol(b: &mut Bits, src: &mut Source, t: *const HCode) -> usize {
    if b.nbits < MAX_CODE_LEN as u32 {
        b.fill(src);
    }
    let v = b.val as u32;
    // A table holds every value of its root bits and of each link's sub bits.
    unsafe {
        let mut e = &*t.add(v as usize & ((1 << ROOT_BITS) - 1));
        if usize::from(e.bits) > ROOT_BITS {
            let sub = u32::from(e.bits) - ROOT_BITS as u32;
            e = &*t.add(usize::from(e.value) + ((v >> ROOT_BITS) & ((1 << sub) - 1)) as usize);
            b.skip(ROOT_BITS as u32 + u32::from(e.bits));
        } else {
            b.skip(u32::from(e.bits));
        }
        usize::from(e.value)
    }
}

#[derive(Clone, Copy)]
struct Group {
    codes: [*const HCode; 5],
}

unsafe impl Zeroable for Group {}

#[derive(Default)]
struct Entropy {
    xs: usize,
    group_bits: u32,
    group_xs: usize,
    group_map: Option<Buffer<u32>>,
    groups: Option<Buffer<Group>>,
    cache: Option<Buffer<u32>>,
    cache_bits: u32,
}

struct Transform {
    kind: u32,
    bits: u32,
    xs: usize,
    data: Option<Buffer<u32>>,
}

/// Lossless image, delivered as ARGB rows. A width of 0 reads the VP8L header for
/// the size; otherwise the stream is a headerless one of that size, as an ALPH
/// chunk carries.
pub(crate) struct Vp8l {
    br: Bits,
    _arena: Arena,
    err: Option<Error>,

    width: u32,
    height: u32,
    has_alpha: bool,

    transforms: Vec<Transform>,
    main: Entropy,

    ring: Buffer<u32>,
    pos: usize,
    total: usize,
    col: usize,
    row: usize,
    out_row: usize,

    tmp: [Buffer<u32>; 2],
    pred: Buffer<u32>,
}

const fn subsample(size: usize, bits: u32) -> usize {
    (size + (1 << bits) - 1) >> bits
}

fn read_code(br: &mut Bits, src: &mut Source, arena: &mut Arena, alphabet: usize, lengths: &mut [u8]) -> Result<*const HCode> {
    let lengths = &mut lengths[..alphabet];
    lengths.fill(0);
    if br.bits(src, 1) != 0 {
        let num = br.bits(src, 1) + 1;
        let first_bits = if br.bits(src, 1) != 0 { 8 } else { 1 };
        let s0 = br.bits(src, first_bits) as usize;
        if s0 < alphabet {
            lengths[s0] = 1;
        }
        if num == 2 {
            let s1 = br.bits(src, 8) as usize;
            if s1 < alphabet {
                lengths[s1] = 1;
            }
        }
    } else {
        let mut cl_lengths = [0u8; 19];
        let num_codes = br.bits(src, 4) as usize + 4;
        for &order in &CODE_LENGTH_ORDER[..num_codes] {
            cl_lengths[usize::from(order)] = br.bits(src, 3) as u8;
        }
        let mark = arena.mark();
        let Some(cl) = build_table(arena, &cl_lengths) else {
            return Err(br.stream_error(src));
        };

        let mut max_symbol = alphabet;
        if br.bits(src, 1) != 0 {
            let nbits = 2 + 2 * br.bits(src, 3);
            max_symbol = 2 + br.bits(src, nbits) as usize;
            if max_symbol > alphabet {
                return Err(Error::Decode);
            }
        }
        let mut prev = 8;
        let mut s = 0;
        while s < alphabet {
            if max_symbol == 0 {
                break;
            }
            max_symbol -= 1;
            let c = read_symbol(br, src, cl);
            if c < 16 {
                lengths[s] = c as u8;
                s += 1;
                if c != 0 {
                    prev = c as u8;
                }
                continue;
            }
            const EXTRA: [u8; 3] = [2, 3, 7];
            const OFFSET: [u8; 3] = [3, 3, 11];
            let repeat = br.bits(src, u32::from(EXTRA[c - 16])) as usize + usize::from(OFFSET[c - 16]);
            if s + repeat > alphabet {
                return Err(Error::Decode);
            }
            lengths[s..s + repeat].fill(if c == 16 { prev } else { 0 });
            s += repeat;
        }
        arena.rewind(mark);
    }
    if br.overrun {
        return Err(br.stream_error(src));
    }
    build_table(arena, lengths).ok_or(Error::Decode)
}

fn read_codes(br: &mut Bits, src: &mut Source, arena: &mut Arena, e: &mut Entropy, xs: usize, ys: usize, level0: bool) -> Result<()> {
    let mut num_groups = 1;
    if level0 && br.bits(src, 1) != 0 {
        let bits = 2 + br.bits(src, 3);
        e.group_bits = bits;
        e.group_xs = subsample(xs, bits);
        let mut map = decode_subimage(br, src, arena, e.group_xs, subsample(ys, bits))?;
        for m in map.iter_mut() {
            *m = (*m >> 8) & 0xffff;
            num_groups = num_groups.max(*m as usize + 1);
        }
        e.group_map = Some(map);
    }

    let mut slot: Buffer<i32> = buffer(num_groups, arena.caps)?;
    let mut used = num_groups;
    if let Some(map) = &mut e.group_map {
        used = 0;
        slot.fill(-1);
        for m in map.iter_mut() {
            let s = &mut slot[*m as usize];
            if *s < 0 {
                *s = used as i32;
                used += 1;
            }
            *m = *s as u32;
        }
    } else {
        slot.iter_mut().enumerate().for_each(|(i, s)| *s = i as i32);
    }
    let mut groups: Buffer<Group> = buffer(used, arena.caps)?;
    let green = NUM_LITERAL + NUM_LENGTH + if e.cache_bits != 0 { 1 << e.cache_bits } else { 0 };
    let mut lengths: Buffer = buffer(green, arena.caps)?;
    for g in 0..num_groups {
        let mark = arena.mark();
        for k in 0..5 {
            let alphabet = match k {
                GREEN => green,
                DIST => NUM_DIST,
                _ => NUM_LITERAL,
            };
            let code = read_code(br, src, arena, alphabet, &mut lengths)?;
            if slot[g] >= 0 {
                groups[slot[g] as usize].codes[k] = code;
            }
        }
        if slot[g] < 0 {
            arena.rewind(mark);
        }
    }
    e.groups = Some(groups);
    Ok(())
}

fn read_entropy(br: &mut Bits, src: &mut Source, arena: &mut Arena, e: &mut Entropy, xs: usize, ys: usize, level0: bool) -> Result<()> {
    e.xs = xs;
    if br.bits(src, 1) != 0 {
        e.cache_bits = br.bits(src, 4);
        if e.cache_bits < 1 || e.cache_bits > MAX_CACHE_BITS {
            return Err(Error::Decode);
        }
    }
    read_codes(br, src, arena, e, xs, ys, level0)?;
    if e.cache_bits != 0 {
        e.cache = Some(buffer(1 << e.cache_bits, arena.caps)?);
    }
    if br.overrun { Err(br.stream_error(src)) } else { Ok(()) }
}

/// A sub-image, decoded whole.
fn decode_subimage(br: &mut Bits, src: &mut Source, arena: &mut Arena, xs: usize, ys: usize) -> Result<Buffer<u32>> {
    let mut e = Entropy::default();
    read_entropy(br, src, arena, &mut e, xs, ys, false)?;
    let total = xs * ys;
    let mut data: Buffer<u32> = buffer(total, arena.caps)?;
    let mut position = Position { pos: 0, col: 0, row: 0 };
    decode_pixels(br, src, &mut e, &mut data, total, &mut position, total)?;
    Ok(data)
}

struct Position {
    pos: usize,
    col: usize,
    row: usize,
}

#[inline(always)]
fn cache_insert(cache: Option<(*mut u32, u32)>, argb: u32) {
    if let Some((p, shift)) = cache {
        unsafe { *p.add((argb.wrapping_mul(0x1e35a7bd) >> shift) as usize) = argb };
    }
}

fn prefix_value(b: &mut Bits, src: &mut Source, symbol: usize) -> usize {
    if symbol < 4 {
        return symbol + 1;
    }
    let extra = ((symbol - 2) >> 1) as u32;
    let offset = (2 + (symbol & 1)) << extra;
    offset + b.bits(src, extra) as usize + 1
}

fn plane_distance(xs: usize, code: usize) -> usize {
    if code > 120 {
        return code - 120;
    }
    let dc = i32::from(CODE_TO_PLANE[code - 1]);
    let dist = (dc >> 4) * xs as i32 + 8 - (dc & 0xf);
    dist.max(1) as usize
}

// Decodes pixels into `ring`, a multiple of the width, until the position reaches
// `target`; a copy may run past it.
fn decode_pixels(b: &mut Bits, src: &mut Source, e: &mut Entropy, ring: &mut [u32], total: usize, position: &mut Position, target: usize) -> Result<()> {
    let ring_size = ring.len();
    let ring = ring.as_mut_ptr();
    let mut p = position.pos;
    let mut ri = p % ring_size;
    let (mut x, mut y) = (position.col, position.row);
    let xs = e.xs;
    let cache = e.cache.as_mut().map(|c| (c.as_mut_ptr(), 32 - e.cache_bits));
    let cache_size = cache.map_or(0, |_| 1 << e.cache_bits);
    let cache_limit = NUM_LITERAL + NUM_LENGTH + cache_size;
    let groups = e.groups.as_ref().unwrap().as_ptr();
    let map = e.group_map.as_ref().map(|m| m.as_ptr());
    let (group_bits, group_xs) = (e.group_bits, e.group_xs);
    while p < target {
        let g = unsafe {
            match map {
                Some(map) => &*groups.add(*map.add((y >> group_bits) * group_xs + (x >> group_bits)) as usize),
                None => &*groups,
            }
        };
        let code = read_symbol(b, src, g.codes[GREEN]);
        if code < NUM_LITERAL {
            let red = read_symbol(b, src, g.codes[RED]) as u32;
            let blue = read_symbol(b, src, g.codes[BLUE]) as u32;
            let alpha = read_symbol(b, src, g.codes[ALPHA]) as u32;
            let argb = (alpha << 24) | (red << 16) | ((code as u32) << 8) | blue;
            unsafe { *ring.add(ri) = argb };
            cache_insert(cache, argb);
            p += 1;
            ri += 1;
            if ri == ring_size {
                ri = 0;
            }
            x += 1;
            if x == xs {
                x = 0;
                y += 1;
            }
        } else if code < NUM_LITERAL + NUM_LENGTH {
            let length = prefix_value(b, src, code - NUM_LITERAL);
            let dist_symbol = read_symbol(b, src, g.codes[DIST]);
            let dist = plane_distance(xs, prefix_value(b, src, dist_symbol));
            if b.overrun {
                break;
            }
            if p < dist || total - p < length || dist > ring_size {
                return Err(Error::Decode);
            }
            let mut si = if ri >= dist { ri - dist } else { ri + ring_size - dist };
            for _ in 0..length {
                unsafe {
                    let argb = *ring.add(si);
                    *ring.add(ri) = argb;
                    cache_insert(cache, argb);
                }
                si += 1;
                if si == ring_size {
                    si = 0;
                }
                ri += 1;
                if ri == ring_size {
                    ri = 0;
                }
            }
            p += length;
            x += length;
            while x >= xs {
                x -= xs;
                y += 1;
            }
        } else if code < cache_limit {
            let (c, _) = cache.unwrap();
            let argb = unsafe { *c.add(code - NUM_LITERAL - NUM_LENGTH) };
            unsafe { *ring.add(ri) = argb };
            p += 1;
            ri += 1;
            if ri == ring_size {
                ri = 0;
            }
            x += 1;
            if x == xs {
                x = 0;
                y += 1;
            }
        } else {
            return Err(Error::Decode);
        }
        if b.overrun {
            break;
        }
    }
    position.pos = p;
    position.col = x;
    position.row = y;
    if b.overrun { Err(b.stream_error(src)) } else { Ok(()) }
}

#[inline(always)]
fn add_pixels(a: u32, b: u32) -> u32 {
    let ag = (a & 0xff00ff00).wrapping_add(b & 0xff00ff00);
    let rb = (a & 0x00ff00ff).wrapping_add(b & 0x00ff00ff);
    (ag & 0xff00ff00) | (rb & 0x00ff00ff)
}

#[inline(always)]
fn average2(a: u32, b: u32) -> u32 {
    (((a ^ b) & 0xfefefefe) >> 1) + (a & b)
}

#[inline(always)]
fn clip255(a: u32) -> u32 {
    if a < 256 { a } else { !a >> 24 }
}

#[inline(always)]
fn sub3(a: i32, b: i32, c: i32) -> i32 {
    (b - c).abs() - (a - c).abs()
}

#[inline(always)]
fn lane(v: u32, s: u32) -> i32 {
    ((v >> s) & 0xff) as i32
}

#[inline(always)]
fn select_pred(a: u32, b: u32, c: u32) -> u32 {
    let sub = |s| sub3(lane(a, s), lane(b, s), lane(c, s));
    if sub(24) + sub(16) + sub(8) + sub(0) <= 0 { a } else { b }
}

#[inline(always)]
fn clamped_add_subtract_full(c0: u32, c1: u32, c2: u32) -> u32 {
    let one = |s| clip255((lane(c0, s) + lane(c1, s) - lane(c2, s)) as u32) << s;
    one(0) | one(8) | one(16) | one(24)
}

#[inline(always)]
fn clamped_add_subtract_half(c0: u32, c1: u32, c2: u32) -> u32 {
    let ave = average2(c0, c1);
    let one = |s| {
        let (a, b) = (lane(ave, s), lane(c2, s));
        clip255((a + (a - b) / 2) as u32) << s
    };
    one(0) | one(8) | one(16) | one(24)
}

#[inline(always)]
unsafe fn predict(mode: u32, left: u32, top: *const u32) -> u32 {
    unsafe {
        let t = || *top;
        let tl = || *top.sub(1);
        let tr = || *top.add(1);
        match mode {
            1 => left,
            2 => t(),
            3 => tr(),
            4 => tl(),
            5 => average2(average2(left, tr()), t()),
            6 => average2(left, tl()),
            7 => average2(left, t()),
            8 => average2(tl(), t()),
            9 => average2(t(), tr()),
            10 => average2(average2(left, tl()), average2(t(), tr())),
            11 => select_pred(t(), left, tl()),
            12 => clamped_add_subtract_full(left, t(), tl()),
            13 => clamped_add_subtract_half(left, t(), tl()),
            _ => 0xff000000,
        }
    }
}

// `pred` holds the previous output row followed by the current one, so the
// top-right of the last pixel is the first pixel of the current row, as the
// format defines it.
unsafe fn inverse_predictor(pred: *mut u32, t: &Transform, y: usize, input: *const u32, out: *mut u32) {
    let w = t.xs;
    unsafe {
        let prev = pred;
        let cur = pred.add(w);
        if y == 0 {
            *cur = add_pixels(*input, 0xff000000);
            for x in 1..w {
                *cur.add(x) = add_pixels(*input.add(x), *cur.add(x - 1));
            }
        } else {
            let data = t.data.as_ref().unwrap();
            let modes = data.as_ptr().add((y >> t.bits) * subsample(w, t.bits));
            *cur = add_pixels(*input, *prev);
            for x in 1..w {
                let mode = (*modes.add(x >> t.bits) >> 8) & 0xf;
                *cur.add(x) = add_pixels(*input.add(x), predict(mode, *cur.add(x - 1), prev.add(x)));
            }
        }
        ptr::copy_nonoverlapping(cur, out, w);
        ptr::copy_nonoverlapping(cur, prev, w);
    }
}

#[inline(always)]
fn color_delta(pred: i8, color: i8) -> i32 {
    (i32::from(pred) * i32::from(color)) >> 5
}

unsafe fn inverse_cross_color(t: &Transform, y: usize, input: *const u32, out: *mut u32) {
    let data = t.data.as_ref().unwrap();
    unsafe {
        let codes = data.as_ptr().add((y >> t.bits) * subsample(t.xs, t.bits));
        for x in 0..t.xs {
            let code = *codes.add(x >> t.bits);
            let argb = *input.add(x);
            let green = (argb >> 8) as i8;
            let mut red = ((argb >> 16) & 0xff) as i32;
            let mut blue = (argb & 0xff) as i32;
            red += color_delta(code as i8, green);
            red &= 0xff;
            blue += color_delta((code >> 8) as i8, green);
            blue += color_delta((code >> 16) as i8, red as i8);
            blue &= 0xff;
            *out.add(x) = (argb & 0xff00ff00) | ((red as u32) << 16) | blue as u32;
        }
    }
}

unsafe fn add_green(t: &Transform, input: *const u32, out: *mut u32) {
    for x in 0..t.xs {
        unsafe {
            let argb = *input.add(x);
            let green = (argb >> 8) & 0xff;
            let rb = (argb & 0x00ff00ff).wrapping_add((green << 16) | green) & 0x00ff00ff;
            *out.add(x) = (argb & 0xff00ff00) | rb;
        }
    }
}

unsafe fn inverse_color_indexing(t: &Transform, mut input: *const u32, out: *mut u32) {
    let colors = t.data.as_ref().unwrap();
    let bpp = 8 >> t.bits;
    if bpp == 8 {
        for x in 0..t.xs {
            unsafe { *out.add(x) = *colors.get_unchecked(((*input.add(x) >> 8) & 0xff) as usize) };
        }
        return;
    }
    let mask = (1u32 << bpp) - 1;
    let per_byte_mask = (1 << t.bits) - 1;
    let mut packed = 0;
    for x in 0..t.xs {
        if x & per_byte_mask == 0 {
            unsafe {
                packed = (*input >> 8) & 0xff;
                input = input.add(1);
            }
        }
        unsafe { *out.add(x) = *colors.get_unchecked((packed & mask) as usize) };
        packed >>= bpp;
    }
}

impl Vp8l {
    pub(crate) fn open(src: &mut Source, pos: usize, len: usize, mut width: u32, mut height: u32, caps: Option<Caps>) -> Result<Self> {
        let mut br = Bits { cursor: Cursor::new(pos, len, WINDOW, caps)?, pos: 0, len: 0, val: 0, nbits: 0, overrun: false };
        let mut has_alpha = false;
        if width == 0 {
            if br.bits(src, 8) != 0x2f {
                return Err(br.stream_error(src));
            }
            width = br.bits(src, 14) + 1;
            height = br.bits(src, 14) + 1;
            has_alpha = br.bits(src, 1) != 0;
            if br.bits(src, 3) != 0 {
                return Err(br.stream_error(src));
            }
        }

        let mut arena = Arena { blocks: Vec::new(), caps };
        let mut transforms = Vec::new();
        transforms.try_reserve_exact(4).map_err(|_| Error::OutOfMemory)?;
        let mut xs = width as usize;
        let ys = height as usize;
        let mut seen = 0u32;
        while br.bits(src, 1) != 0 {
            let kind = br.bits(src, 2);
            if seen & (1 << kind) != 0 {
                return Err(Error::Decode);
            }
            seen |= 1 << kind;
            let mut t = Transform { kind, bits: 0, xs, data: None };
            if kind == PREDICTOR || kind == CROSS_COLOR {
                t.bits = 2 + br.bits(src, 3);
                t.data = Some(decode_subimage(&mut br, src, &mut arena, subsample(xs, t.bits), subsample(ys, t.bits))?);
            } else if kind == COLOR_INDEXING {
                let num_colors = br.bits(src, 8) as usize + 1;
                t.bits = match num_colors {
                    17.. => 0,
                    5.. => 1,
                    3.. => 2,
                    _ => 3,
                };
                xs = subsample(xs, t.bits);
                let colors = decode_subimage(&mut br, src, &mut arena, num_colors, 1)?;
                let mut table: Buffer<u32> = buffer(1 << (8 >> t.bits), caps)?;
                table[0] = colors[0];
                for i in 1..num_colors {
                    table[i] = add_pixels(colors[i], table[i - 1]);
                }
                t.data = Some(table);
            }
            transforms.push(t);
        }
        let mut main = Entropy::default();
        read_entropy(&mut br, src, &mut arena, &mut main, xs, ys, true)?;

        let total = xs * ys;
        let need = (7 * xs + 8).max(MAX_FAR_DIST).max(xs + MAX_COPY);
        let ring_size = need.div_ceil(xs) * xs;
        let width_px = width as usize;
        Ok(Self {
            br,
            _arena: arena,
            err: None,
            width,
            height,
            has_alpha,
            transforms,
            main,
            ring: buffer(ring_size.min(total), caps)?,
            pos: 0,
            total,
            col: 0,
            row: 0,
            out_row: 0,
            tmp: [buffer(width_px, caps)?, buffer(width_px, caps)?],
            pred: buffer(width_px * 2, caps)?,
        })
    }

    pub(crate) const fn width(&self) -> u32 {
        self.width
    }

    pub(crate) const fn height(&self) -> u32 {
        self.height
    }

    pub(crate) const fn has_alpha(&self) -> bool {
        self.has_alpha
    }

    pub(crate) fn next_row(&mut self, src: &mut Source, argb: &mut [u32]) -> Result<()> {
        if let Some(e) = self.err {
            return Err(e);
        }
        if self.out_row >= self.height as usize {
            return Err(Error::InvalidState);
        }
        let xs = self.main.xs;
        let end = (self.out_row + 1) * xs;
        if self.pos < end {
            let mut position = Position { pos: self.pos, col: self.col, row: self.row };
            let result = decode_pixels(&mut self.br, src, &mut self.main, &mut self.ring, self.total, &mut position, end);
            self.pos = position.pos;
            self.col = position.col;
            self.row = position.row;
            if let Err(e) = result {
                self.err = Some(e);
                return Err(e);
            }
        }
        let mut input = self.ring[(self.out_row * xs) % self.ring.len()..].as_ptr();
        let dst = argb.as_mut_ptr();
        // `tmp` and `argb` hold a row of the picture, and the ring holds `xs` here.
        for i in (0..self.transforms.len()).rev() {
            let t = &self.transforms[i];
            let out = if i == 0 { dst } else { self.tmp[i & 1].as_mut_ptr() };
            unsafe {
                match t.kind {
                    PREDICTOR => inverse_predictor(self.pred.as_mut_ptr(), t, self.out_row, input, out),
                    CROSS_COLOR => inverse_cross_color(t, self.out_row, input, out),
                    SUBTRACT_GREEN => add_green(t, input, out),
                    _ => inverse_color_indexing(t, input, out),
                }
            }
            input = out;
        }
        if self.transforms.is_empty() {
            let width = self.width as usize;
            argb[..width].copy_from_slice(&self.ring[(self.out_row * xs) % self.ring.len()..][..width]);
        }
        self.out_row += 1;
        Ok(())
    }
}
