/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::alloc::Allocator;
use core::ptr;

use devkit_gfx::ImageBuf;
use idf::heap::Caps;

use crate::mem::{Buffer, buffer, default_caps};
use crate::pixel::{pack_row, store_ycc_row, unpack_row};
use crate::planes::{PlaneLayout, Planes};
use crate::{Decoder, Error, ImageFormat, ImageInfo, Result, is_row_format};

const Q16: u32 = 65536;

// Rounding once per axis biases the result whenever both roundings land on a tie,
// which an integer ratio hits constantly, so a box vertical pass gets four
// fractional bits and does the only rounding.
const HROW_FRAC_BITS: u32 = 4;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Fit {
    /// Exactly the target size.
    #[default]
    Stretch,
    /// Aspect preserved, inside the target. A target of 0 leaves that axis
    /// unbounded; both 0 keep the source size.
    Contain,
}

/// The eight TIFF/EXIF orientations, in their order. Rotations are clockwise.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Orientation {
    #[default]
    None,
    MirrorX,
    Rot180,
    MirrorY,
    Transpose,
    Rot90,
    Transverse,
    Rot270,
}

impl Orientation {
    pub const fn swaps_axes(self) -> bool {
        matches!(self, Self::Transpose | Self::Rot90 | Self::Transverse | Self::Rot270)
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct ResizeOptions {
    /// With an orientation, the size of the oriented picture.
    pub target_width: u16,
    pub target_height: u16,
    pub fit: Fit,
    /// `None` keeps the source format.
    pub format: Option<ImageFormat>,
    pub caps: Option<Caps>,
}

fn unoriented_size(src_w: u16, src_h: u16, options: &ResizeOptions, orientation: Orientation) -> Result<(u16, u16)> {
    if src_w == 0 || src_h == 0 {
        return Err(Error::InvalidArgument);
    }
    let swap = orientation.swaps_axes();
    let (sw, sh) = (u32::from(src_w), u32::from(src_h));
    let tw = u32::from(if swap { options.target_height } else { options.target_width });
    let th = u32::from(if swap { options.target_width } else { options.target_height });
    let (dw, dh);
    if options.fit == Fit::Stretch {
        if tw == 0 || th == 0 {
            return Err(Error::InvalidArgument);
        }
        (dw, dh) = (tw, th);
    } else if tw == 0 && th == 0 {
        (dw, dh) = (sw, sh);
    } else {
        // Every product here is bounded by 65535 * 65535, so 32 bits are enough:
        // a 64-bit divide is a library call on these chips.
        if th == 0 || (tw != 0 && tw * sh <= th * sw) {
            dw = tw;
            dh = ((sh * tw + sw / 2) / sw).max(1);
        } else {
            dh = th;
            dw = ((sw * th + sh / 2) / sh).max(1);
        }
    }
    if dw > 0xFFFF || dh > 0xFFFF {
        return Err(Error::TooLarge);
    }
    Ok((dw as u16, dh as u16))
}

/// The output size, oriented, without allocating.
pub fn output_size(src_w: u16, src_h: u16, options: &ResizeOptions, orientation: Orientation) -> Result<(u16, u16)> {
    let (dw, dh) = unoriented_size(src_w, src_h, options, orientation)?;
    Ok(if orientation.swaps_axes() { (dh, dw) } else { (dw, dh) })
}

/// Box-area weights for one axis, differences of a Q16 prefix sum: they add up to
/// exactly Q16 whatever the tap count, which lets the row pass end with a shift.
struct BoxTable {
    off: Buffer<u32>,
    src: Buffer<u32>,
    weights: Buffer<u32>,
}

impl BoxTable {
    fn new(s: u32, d: u32, caps: Option<Caps>) -> Result<Self> {
        let mut off: Buffer<u32> = buffer(d as usize + 1, caps)?;
        let mut src: Buffer<u32> = buffer(d as usize, caps)?;
        for x in 0..d {
            let sx0 = (x * s) / d;
            let sx1 = ((x + 1) * s + d - 1) / d;
            let sx1 = sx1.min(s).max(sx0 + 1);
            src[x as usize] = sx0;
            off[x as usize + 1] = off[x as usize] + (sx1 - sx0);
        }
        let mut weights: Buffer<u32> = buffer(off[d as usize] as usize, caps)?;
        for x in 0..d {
            let (u0, u1) = (x * s, (x + 1) * s);
            let sx0 = src[x as usize];
            let first = off[x as usize];
            let cnt = off[x as usize + 1] - first;
            let (mut cum, mut wsum) = (0u32, 0u32);
            for i in 0..cnt {
                let k0 = (sx0 + i) * d;
                let (lo, hi) = (u0.max(k0), u1.min(k0 + d));
                if hi > lo {
                    cum += hi - lo;
                }
                let ws = (cum * Q16) / s;
                weights[(first + i) as usize] = ws - wsum;
                wsum = ws;
            }
        }
        Ok(Self { off, src, weights })
    }
}

impl BoxTable {
    /// `BoxTable` for samples that need not be alike: source sample `j` covers
    /// `[src(j), src(j + 1))` and destination sample `x` covers
    /// `[dst(x), dst(x + 1))`, both on one scale from 0 to the same end.
    fn spans(n_src: usize, src: impl Fn(usize) -> u64, n_dst: usize, dst: impl Fn(usize) -> u64, caps: Option<Caps>) -> Result<Self> {
        let mut off: Buffer<u32> = buffer(n_dst + 1, caps)?;
        let mut first: Buffer<u32> = buffer(n_dst, caps)?;
        let mut j = 0;
        for x in 0..n_dst {
            let (lo, hi) = (dst(x), dst(x + 1));
            while j + 1 < n_src && src(j + 1) <= lo {
                j += 1;
            }
            let mut k = j;
            while k + 1 < n_src && src(k + 1) < hi {
                k += 1;
            }
            first[x] = j as u32;
            off[x + 1] = off[x] + (k - j + 1) as u32;
        }
        let mut weights: Buffer<u32> = buffer(off[n_dst] as usize, caps)?;
        for x in 0..n_dst {
            let (lo, hi) = (dst(x), dst(x + 1));
            let (mut cum, mut wsum) = (0u64, 0u32);
            for i in off[x]..off[x + 1] {
                let k = first[x] as usize + (i - off[x]) as usize;
                let (a, b) = (lo.max(src(k)), hi.min(src(k + 1)));
                if b > a {
                    cum += b - a;
                }
                let ws = (cum * u64::from(Q16) / (hi - lo)) as u32;
                weights[i as usize] = ws - wsum;
                wsum = ws;
            }
        }
        Ok(Self { off, src: first, weights })
    }
}

enum ChromaAxis {
    Box(BoxTable),
    Linear(Bilinear),
}

impl ChromaAxis {
    /// `s` luma samples, a chroma sample covering `1 << shift` of them, to `d`
    /// outputs placed where the luma resize of this axis puts them.
    fn new(s: usize, shift: u32, d: usize, caps: Option<Caps>) -> Result<Self> {
        let n = s.div_ceil(1 << shift);
        let (s64, d64) = (s as u64, d as u64);
        if d <= n {
            return Ok(Self::Box(BoxTable::spans(n, |j| ((j << shift).min(s) as u64) * d64, d, |x| x as u64 * s64, caps)?));
        }
        // Positions in luma sample centres, scaled to stay integral: the luma
        // resize box-averages where it shrinks and joins the corners where it grows.
        let u = 1i64 << shift;
        let (s, d) = (s as i64, d as i64);
        let (scale, at): (i64, &dyn Fn(usize) -> i64) = if d <= s {
            (2 * d, &|x| (2 * x as i64 + 1) * s - d)
        } else if d > 1 {
            (2 * (d - 1), &|x| 2 * x as i64 * (s - 1))
        } else {
            (2, &|_| s - 1)
        };
        let centre = |j: usize| (2 * j as i64 * u + u - 1) * scale / 2;
        Ok(Self::Linear(Bilinear::at(n, centre, d as usize, at, caps)?))
    }

    fn len(&self) -> usize {
        match self {
            Self::Box(t) => t.src.len(),
            Self::Linear(t) => t.lo.len(),
        }
    }
}

/// Resizes the two chroma planes to one sample per output pixel. A chroma
/// sample covers the picture unevenly where it has an odd size.
struct ChromaScaler {
    dw: usize,
    horizontal: ChromaAxis,
    vertical: ChromaAxis,
    /// The latest source row of each plane after the horizontal pass, and the
    /// one before for a growing vertical axis.
    hrow: [[Buffer<u16>; 2]; 2],
    acc: [[Buffer<u32>; 3]; 2],
    out: [Buffer; 2],
    src_y: usize,
    dst_y: usize,
}

impl ChromaScaler {
    /// The picture is `size` pixels and a chroma sample covers `1 << shift`
    /// of them.
    fn new(size: (usize, usize), shift: (u32, u32), out: (usize, usize), caps: Option<Caps>) -> Result<Self> {
        let horizontal = ChromaAxis::new(size.0, shift.0, out.0, caps)?;
        let vertical = ChromaAxis::new(size.1, shift.1, out.1, caps)?;
        let dw = out.0;
        let acc = || -> Result<[Buffer<u32>; 3]> { Ok([buffer(dw, caps)?, buffer(dw, caps)?, buffer(dw, caps)?]) };
        let hrow = || -> Result<[Buffer<u16>; 2]> { Ok([buffer(dw, caps)?, buffer(dw, caps)?]) };
        Ok(Self {
            dw,
            horizontal,
            vertical,
            hrow: [hrow()?, hrow()?],
            acc: [acc()?, acc()?],
            out: [buffer(dw + ROW_SLACK, caps)?, buffer(dw + ROW_SLACK, caps)?],
            src_y: 0,
            dst_y: 0,
        })
    }

    /// Takes the next source rows of Cb and Cr and hands `done` each pair of
    /// destination rows they complete, in order.
    fn push(&mut self, rows: [&[u8]; 2], mut done: impl FnMut(&[u8], &[u8]) -> Result<()>) -> Result<()> {
        let shift = 16 - HROW_FRAC_BITS;
        let round = 1 << (shift - 1);
        for (row, hrow) in rows.iter().zip(&mut self.hrow) {
            hrow.swap(0, 1);
            match &self.horizontal {
                ChromaAxis::Box(t) => hreduce_box::<u8, 1>(row, self.dw, &mut hrow[0], t, shift, round),
                ChromaAxis::Linear(t) => hexpand_bilinear::<u8, 1>(row, self.dw, &mut hrow[0], t, shift, round),
            }
        }
        let r = self.src_y as u32;
        let rows = self.vertical.len();
        match &self.vertical {
            ChromaAxis::Box(t) => {
                let mut y = self.dst_y;
                while y < rows && t.src[y] <= r {
                    let i = t.off[y] + (r - t.src[y]);
                    if i < t.off[y + 1] {
                        let w = t.weights[i as usize];
                        for (acc, hrow) in self.acc.iter_mut().zip(&self.hrow) {
                            for (a, &h) in acc[y % 3].iter_mut().zip(&hrow[0][..]) {
                                *a += u32::from(h) * w;
                            }
                        }
                    }
                    y += 1;
                }
                while self.dst_y < rows && t.src[self.dst_y] + (t.off[self.dst_y + 1] - t.off[self.dst_y]) <= r + 1 {
                    for (acc, out) in self.acc.iter_mut().zip(&mut self.out) {
                        for (o, a) in out.iter_mut().zip(acc[self.dst_y % 3].iter_mut()) {
                            *o = ((*a + (1 << (VOUT_SHIFT - 1))) >> VOUT_SHIFT).min(255) as u8;
                            *a = 0;
                        }
                    }
                    done(&self.out[0], &self.out[1])?;
                    self.dst_y += 1;
                }
            }
            ChromaAxis::Linear(t) => {
                while self.dst_y < rows && t.hi[self.dst_y] <= r {
                    let y = self.dst_y;
                    let (lo, b) = (t.lo[y], t.blend[y]);
                    let inv = Q16 - b;
                    for (hrow, out) in self.hrow.iter().zip(&mut self.out) {
                        let low = if lo == r { &hrow[0] } else { &hrow[1] };
                        for ((o, &l), &h) in out.iter_mut().zip(&low[..self.dw]).zip(&hrow[0][..]) {
                            let v = (u32::from(l) * inv + u32::from(h) * b + (1 << (VOUT_SHIFT - 1))) >> VOUT_SHIFT;
                            *o = v.min(255) as u8;
                        }
                    }
                    done(&self.out[0], &self.out[1])?;
                    self.dst_y += 1;
                }
            }
        }
        self.src_y += 1;
        Ok(())
    }
}

struct Bilinear {
    lo: Buffer<u32>,
    hi: Buffer<u32>,
    blend: Buffer<u32>,
}

impl Bilinear {
    /// Between the source samples whose positions `src(j)` straddle each
    /// destination's `dst(x)`, held at the ends; positions only need to grow
    /// with their index, on one scale.
    fn at(n_src: usize, src: impl Fn(usize) -> i64, n_dst: usize, dst: impl Fn(usize) -> i64, caps: Option<Caps>) -> Result<Self> {
        let mut t = Self { lo: buffer(n_dst, caps)?, hi: buffer(n_dst, caps)?, blend: buffer(n_dst, caps)? };
        let last = n_src - 1;
        let mut j = 0;
        for x in 0..n_dst {
            let p = dst(x);
            while j < last && src(j + 1) <= p {
                j += 1;
            }
            let (lo, hi, blend) = if p <= src(0) {
                (0, 0, 0)
            } else if j == last {
                (last, last, 0)
            } else {
                let (a, b) = (src(j), src(j + 1));
                (j, j + 1, (((p - a) * i64::from(Q16) + (b - a) / 2) / (b - a)) as u32)
            };
            (t.lo[x], t.hi[x], t.blend[x]) = (lo as u32, hi as u32, blend);
        }
        Ok(t)
    }

    fn new(s: u32, d: u32, caps: Option<Caps>) -> Result<Self> {
        let mut t = Self { lo: buffer(d as usize, caps)?, hi: buffer(d as usize, caps)?, blend: buffer(d as usize, caps)? };
        for x in 0..d {
            let (num, den) = if s == 1 {
                (0, 1)
            } else if d == 1 {
                (s - 1, 2)
            } else {
                (x * (s - 1), d - 1)
            };
            let xl = num / den;
            let rem = num % den;
            t.lo[x as usize] = xl;
            t.hi[x as usize] = if xl + 1 < s { xl + 1 } else { xl };
            t.blend[x as usize] = (rem * Q16 + den / 2) / den;
        }
        Ok(t)
    }
}

#[inline(always)]
fn hreduce_box<T: Copy + Into<u32>, const CH: usize>(row: &[T], dw: usize, hrow: &mut [u16], t: &BoxTable, shift: u32, round: u32) {
    debug_assert!(hrow.len() >= dw * CH && t.src.len() >= dw && t.off.len() > dw);
    debug_assert!(t.off[dw] as usize <= t.weights.len());
    let rp = row.as_ptr();
    let (mut wp, off, src) = (t.weights.as_ptr(), t.off.as_ptr(), t.src.as_ptr());
    let mut out = hrow.as_mut_ptr();
    let mut first = 0;
    // The table was built for this row width, so every index stays inside.
    unsafe {
        for x in 0..dw {
            let next = *off.add(x + 1);
            let cnt = (next - first) as usize;
            first = next;
            let mut p = rp.add(*src.add(x) as usize * CH);
            let at = |p: *const T, i: usize| -> u32 { (*p.add(i)).into() };
            if CH == 3 {
                let (mut s0, mut s1, mut s2);
                if cnt == 2 {
                    let (w0, w1) = (*wp, *wp.add(1));
                    s0 = at(p, 0) * w0 + at(p, 3) * w1;
                    s1 = at(p, 1) * w0 + at(p, 4) * w1;
                    s2 = at(p, 2) * w0 + at(p, 5) * w1;
                } else {
                    (s0, s1, s2) = (0, 0, 0);
                    for i in 0..cnt {
                        let wi = *wp.add(i);
                        s0 = s0.wrapping_add(at(p, 0) * wi);
                        s1 = s1.wrapping_add(at(p, 1) * wi);
                        s2 = s2.wrapping_add(at(p, 2) * wi);
                        p = p.add(3);
                    }
                }
                *out = ((s0 + round) >> shift) as u16;
                *out.add(1) = ((s1 + round) >> shift) as u16;
                *out.add(2) = ((s2 + round) >> shift) as u16;
            } else {
                let mut s0 = 0u32;
                for i in 0..cnt {
                    s0 = s0.wrapping_add(at(p, i) * *wp.add(i));
                }
                *out = ((s0 + round) >> shift) as u16;
            }
            wp = wp.add(cnt);
            out = out.add(CH);
        }
    }
}

#[inline(always)]
fn hexpand_bilinear<T: Copy + Into<u32>, const CH: usize>(row: &[T], dw: usize, hrow: &mut [u16], t: &Bilinear, shift: u32, round: u32) {
    let rp = row.as_ptr();
    let mut out = hrow.as_mut_ptr();
    unsafe {
        for x in 0..dw {
            let b = *t.blend.get_unchecked(x);
            let inv = Q16 - b;
            let pl = rp.add(*t.lo.get_unchecked(x) as usize * CH);
            let ph = rp.add(*t.hi.get_unchecked(x) as usize * CH);
            for c in 0..CH {
                let l: u32 = (*pl.add(c)).into();
                let h: u32 = (*ph.add(c)).into();
                *out.add(c) = ((l * inv + h * b + round) >> shift) as u16;
            }
            out = out.add(CH);
        }
    }
}

fn horizontal<T: Copy + Into<u32>>(row: &[T], ch: usize, dw: usize, hrow: &mut [u16], h: &Horizontal, shift: u32, round: u32) {
    match (h, ch) {
        (Horizontal::Expand(t), 3) => hexpand_bilinear::<T, 3>(row, dw, hrow, t, shift, round),
        (Horizontal::Expand(t), _) => hexpand_bilinear::<T, 1>(row, dw, hrow, t, shift, round),
        (Horizontal::Reduce(t), 3) => hreduce_box::<T, 3>(row, dw, hrow, t, shift, round),
        (Horizontal::Reduce(t), _) => hreduce_box::<T, 1>(row, dw, hrow, t, shift, round),
    }
}

const VOUT_SHIFT: u32 = 16 + HROW_FRAC_BITS;

enum Horizontal {
    Reduce(BoxTable),
    Expand(Bilinear),
}

struct VerticalUp {
    table: Bilinear,
    hrow_prev: Buffer<u16>,
    ready: usize,
}

/// The dst row being accumulated spans `[span_lo, span_hi)` in 1/dh source rows,
/// from the start of the src row about to be pushed.
struct VerticalDown {
    acc: Buffer<u32>,
    span_lo: i32,
    span_hi: i32,
    cum: u32,
    wsum: u32,
    acc_dst_y: usize,
    pending: Buffer,
    pending_avail: bool,
}

enum Vertical {
    Up(VerticalUp),
    Down(VerticalDown),
}

struct Core {
    sw: usize,
    sh: usize,
    dw: usize,
    dh: usize,
    src_format: ImageFormat,
    dst_format: ImageFormat,
    ch: usize,
    src_direct: bool,
    src_bytes: usize,
    dst_bytes: usize,
    h_shift: u32,
    h_round: u32,
    horizontal: Horizontal,
    vertical: Vertical,
    irow: Option<Buffer<u16>>,
    hrow: Buffer<u16>,
    // A finished row is built here before pack_row, never in hrow: a src row that
    // straddles a dst boundary is still needed for the next dst row.
    vtmp: Buffer<u16>,
    src_y: usize,
    dst_y: usize,
    finished: bool,
}

impl Core {
    fn new(info: ImageInfo, options: &ResizeOptions, orientation: Orientation) -> Result<Self> {
        let (dw16, dh16) = unoriented_size(info.width, info.height, options, orientation)?;
        let (sw, sh) = (usize::from(info.width), usize::from(info.height));
        let (dw, dh) = (usize::from(dw16), usize::from(dh16));
        let src_format = info.format;
        let dst_format = options.format.unwrap_or(src_format);
        if !is_row_format(src_format) || !is_row_format(dst_format) {
            return Err(Error::Unsupported);
        }
        let caps = options.caps;
        let ch = if src_format == ImageFormat::L8 && dst_format == ImageFormat::L8 { 1 } else { 3 };
        let src_direct = src_format == ImageFormat::Rgb888 && ch == 3 || src_format == ImageFormat::L8 && ch == 1;
        let h_up = dw > sw;
        let v_up = dh > sh;
        let h_shift = if v_up { 16 } else { 16 - HROW_FRAC_BITS };
        let dim = dw * ch;
        let horizontal = if h_up {
            Horizontal::Expand(Bilinear::new(sw as u32, dw as u32, caps)?)
        } else {
            Horizontal::Reduce(BoxTable::new(sw as u32, dw as u32, caps)?)
        };
        let vertical = if v_up {
            Vertical::Up(VerticalUp {
                table: Bilinear::new(sh as u32, dh as u32, caps)?,
                hrow_prev: buffer(dim, caps)?,
                ready: 0,
            })
        } else {
            Vertical::Down(VerticalDown {
                acc: buffer(dim, caps)?,
                span_lo: 0,
                span_hi: sh as i32,
                cum: 0,
                wsum: 0,
                acc_dst_y: 0,
                pending: buffer(dst_format.row_bytes(dw), caps)?,
                pending_avail: false,
            })
        };
        Ok(Self {
            sw,
            sh,
            dw,
            dh,
            src_format,
            dst_format,
            ch,
            src_direct,
            src_bytes: src_format.row_bytes(sw),
            dst_bytes: dst_format.row_bytes(dw),
            h_shift,
            h_round: 1 << (h_shift - 1),
            horizontal,
            vertical,
            irow: if src_direct { None } else { Some(buffer(sw * ch, caps)?) },
            hrow: buffer(dim, caps)?,
            vtmp: buffer(dim, caps)?,
            src_y: 0,
            dst_y: 0,
            finished: false,
        })
    }

    /// How many dst rows became ready.
    fn push_row(&mut self, src_row: &[u8]) -> Result<usize> {
        if self.finished || self.src_y >= self.sh {
            return Err(Error::InvalidState);
        }
        if src_row.len() < self.src_bytes {
            return Err(Error::InvalidArgument);
        }
        let sy = self.src_y;
        if let Vertical::Up(up) = &mut self.vertical {
            core::mem::swap(&mut self.hrow, &mut up.hrow_prev);
        }

        let (ch, dw, shift, round) = (self.ch, self.dw, self.h_shift, self.h_round);
        if self.src_direct {
            horizontal(src_row, ch, dw, &mut self.hrow, &self.horizontal, shift, round);
        } else {
            let irow = self.irow.as_mut().unwrap();
            unpack_row(src_row, irow, self.sw, self.src_format, ch);
            horizontal(&irow[..], ch, dw, &mut self.hrow, &self.horizontal, shift, round);
        }

        let produced = match &mut self.vertical {
            Vertical::Up(up) => {
                let mut newly = 0;
                while up.ready < self.dh && up.table.hi[up.ready] as usize <= sy {
                    up.ready += 1;
                    newly += 1;
                }
                newly
            }
            Vertical::Down(down) => {
                let (dw, dh, sh, ch, format) = (self.dw, self.dh as i32, self.sh as u32, self.ch, self.dst_format);
                let dim = dw * ch;
                let mut produced = 0;
                if down.acc_dst_y < self.dh {
                    v_charge(down, &self.hrow[..dim], dh, sh);
                    if down.span_hi <= dh {
                        for (t, &a) in self.vtmp[..dim].iter_mut().zip(&down.acc[..dim]) {
                            *t = ((a + (1 << (VOUT_SHIFT - 1))) >> VOUT_SHIFT) as u16;
                        }
                        pack_row(&self.vtmp, &mut down.pending, dw, format, ch);
                        down.pending_avail = true;
                        down.acc[..dim].fill(0);
                        down.acc_dst_y += 1;
                        down.span_lo = down.span_hi;
                        down.span_hi += sh as i32;
                        down.cum = 0;
                        down.wsum = 0;
                        produced = 1;
                        if down.acc_dst_y < self.dh {
                            v_charge(down, &self.hrow[..dim], dh, sh);
                        }
                    }
                }
                down.span_lo -= dh;
                down.span_hi -= dh;
                produced
            }
        };
        self.src_y = sy + 1;
        Ok(produced)
    }

    fn pop(&mut self, dst_row: &mut [u8]) -> bool {
        if self.dst_y >= self.dh {
            return false;
        }
        let dim = self.dw * self.ch;
        match &mut self.vertical {
            Vertical::Up(up) => {
                let dy = self.dst_y;
                if dy >= up.ready {
                    return false;
                }
                let curr = self.src_y - 1;
                let lo = up.table.lo[dy] as usize;
                let hi = up.table.hi[dy] as usize;
                let blend = up.table.blend[dy];
                let p_lo: &[u16] = if lo == curr { &self.hrow } else { &up.hrow_prev };
                let p_hi: &[u16] = if hi == curr { &self.hrow } else { &up.hrow_prev };
                let src: &[u16] = if lo == hi || blend == 0 {
                    p_lo
                } else if blend >= Q16 {
                    p_hi
                } else {
                    let inv = Q16 - blend;
                    for ((t, &l), &h) in self.vtmp[..dim].iter_mut().zip(p_lo).zip(p_hi) {
                        *t = ((u32::from(l) * inv + u32::from(h) * blend + Q16 / 2) >> 16) as u16;
                    }
                    &self.vtmp
                };
                pack_row(src, dst_row, self.dw, self.dst_format, self.ch);
                self.dst_y = dy + 1;
                true
            }
            Vertical::Down(down) => {
                if !down.pending_avail {
                    return false;
                }
                dst_row[..self.dst_bytes].copy_from_slice(&down.pending[..self.dst_bytes]);
                down.pending_avail = false;
                self.dst_y += 1;
                true
            }
        }
    }

    fn finish(&mut self) -> usize {
        if self.finished {
            return 0;
        }
        self.finished = true;
        let Vertical::Down(down) = &mut self.vertical else {
            return 0;
        };
        if down.acc_dst_y >= self.dh || down.wsum == 0 {
            return 0;
        }
        let dim = self.dw * self.ch;
        let den = down.wsum << HROW_FRAC_BITS;
        for (t, &a) in self.vtmp[..dim].iter_mut().zip(&down.acc[..dim]) {
            *t = ((a + den / 2) / den) as u16;
        }
        pack_row(&self.vtmp, &mut down.pending, self.dw, self.dst_format, self.ch);
        down.pending_avail = true;
        down.acc_dst_y += 1;
        down.acc[..dim].fill(0);
        down.wsum = 0;
        down.cum = 0;
        1
    }
}

fn v_charge(down: &mut VerticalDown, hrow: &[u16], dh: i32, sh: u32) {
    let lo = down.span_lo.max(0);
    let hi = down.span_hi.min(dh);
    if hi <= lo {
        return;
    }
    down.cum += (hi - lo) as u32;
    let ws = (down.cum * Q16) / sh;
    let wi = ws - down.wsum;
    down.wsum = ws;
    for (a, &h) in down.acc.iter_mut().zip(hrow) {
        *a = a.wrapping_add(u32::from(h) * wi);
    }
}

/// Box-area average downscale and bilinear upscale, per axis, converting pixel
/// formats in passing. Push source rows in order and pop what each makes ready.
pub struct Resizer {
    core: Core,
}

impl Resizer {
    pub fn new(info: ImageInfo, options: &ResizeOptions) -> Result<Self> {
        Ok(Self { core: Core::new(info, options, Orientation::None)? })
    }

    pub fn info(&self) -> ImageInfo {
        ImageInfo { width: self.core.dw as u16, height: self.core.dh as u16, format: self.core.dst_format }
    }

    /// The rows that are ready must be popped before the next push, which the
    /// borrow enforces.
    pub fn push_row(&mut self, row: &[u8]) -> Result<Ready<'_>> {
        let count = self.core.push_row(row)?;
        Ok(Ready { core: &mut self.core, count })
    }

    /// Ends the input, which may complete one last row.
    pub fn finish(mut self) -> Finished {
        let count = self.core.finish();
        Finished { core: self.core, count }
    }
}

#[must_use]
pub struct Ready<'a> {
    core: &'a mut Core,
    count: usize,
}

impl Ready<'_> {
    pub const fn count(&self) -> usize {
        self.count
    }

    /// Writes the next ready row; `false` when there are no more.
    pub fn pop(&mut self, dst: &mut [u8]) -> Result<bool> {
        pop_counted(self.core, &mut self.count, dst)
    }
}

pub struct Finished {
    core: Core,
    count: usize,
}

impl Finished {
    pub const fn count(&self) -> usize {
        self.count
    }

    pub fn pop(&mut self, dst: &mut [u8]) -> Result<bool> {
        pop_counted(&mut self.core, &mut self.count, dst)
    }
}

fn pop_counted(core: &mut Core, count: &mut usize, dst: &mut [u8]) -> Result<bool> {
    if *count == 0 {
        return Ok(false);
    }
    if dst.len() < core.dst_bytes {
        return Err(Error::InvalidArgument);
    }
    *count -= 1;
    Ok(core.pop(dst))
}

/// `Resizer` that turns or mirrors the picture on its way out. A row may land as a
/// column, so every pop takes the whole destination.
pub struct OrientedResizer {
    core: Core,
    orientation: Orientation,
    row: Buffer,
}

impl OrientedResizer {
    pub fn new(info: ImageInfo, options: &ResizeOptions, orientation: Orientation) -> Result<Self> {
        let core = Core::new(info, options, orientation)?;
        let row_bytes = if orientation == Orientation::None { 0 } else { core.dst_bytes };
        let row = buffer(row_bytes, options.caps)?;
        Ok(Self { core, orientation, row })
    }

    /// The size after orientation.
    pub fn info(&self) -> ImageInfo {
        let (w, h) = (self.core.dw as u16, self.core.dh as u16);
        let (width, height) = if self.orientation.swaps_axes() { (h, w) } else { (w, h) };
        ImageInfo { width, height, format: self.core.dst_format }
    }

    pub fn push_row(&mut self, row: &[u8]) -> Result<OrientedReady<'_>> {
        let count = self.core.push_row(row)?;
        Ok(OrientedReady { r: self, count })
    }

    pub fn finish(mut self) -> OrientedFinished {
        let count = self.core.finish();
        OrientedFinished { r: self, count }
    }

    fn pop_into(&mut self, count: &mut usize, dst: &mut [u8], stride: usize) -> Result<bool> {
        if *count == 0 {
            return Ok(false);
        }
        let info = self.info();
        let row_bytes = info.row_bytes();
        if stride < row_bytes || dst.len() < (usize::from(info.height) - 1) * stride + row_bytes {
            return Err(Error::InvalidArgument);
        }
        *count -= 1;
        let y = self.core.dst_y;
        if self.orientation == Orientation::None {
            return Ok(self.core.pop(&mut dst[y * stride..y * stride + row_bytes]));
        }
        if !self.core.pop(&mut self.row) {
            return Ok(false);
        }
        unsafe { self.place_row(y, dst.as_mut_ptr(), stride) };
        Ok(true)
    }

    unsafe fn place_row(&self, y: usize, dst: *mut u8, stride: usize) {
        let (w, h, bpp) = (self.core.dw, self.core.dh, self.core.dst_format.bits() / 8);
        unsafe { place_row(self.orientation, w, h, bpp, &self.row, y, dst, stride) };
    }
}

/// Row `y` of a `w` by `h` picture into its oriented place in `dst`.
unsafe fn place_row(orientation: Orientation, w: usize, h: usize, bpp: usize, row: &[u8], y: usize, dst: *mut u8, stride: usize) {
    let (w, h, bpp) = (w as isize, h as isize, bpp as isize);
    let (y, s) = (y as isize, stride as isize);
    let (at, step) = match orientation {
        Orientation::None => (y * s, bpp),
        Orientation::MirrorX => (y * s + (w - 1) * bpp, -bpp),
        Orientation::Rot180 => ((h - 1 - y) * s + (w - 1) * bpp, -bpp),
        Orientation::MirrorY => ((h - 1 - y) * s, bpp),
        Orientation::Transpose => (y * bpp, s),
        Orientation::Rot90 => ((h - 1 - y) * bpp, s),
        Orientation::Transverse => ((w - 1) * s + (h - 1 - y) * bpp, -s),
        Orientation::Rot270 => ((w - 1) * s + y * bpp, -s),
    };
    let mut d = unsafe { dst.offset(at) };
    for px in row[..(w * bpp) as usize].chunks_exact(bpp as usize) {
        unsafe {
            ptr::copy_nonoverlapping(px.as_ptr(), d, bpp as usize);
            d = d.offset(step);
        }
    }
}

#[must_use]
pub struct OrientedReady<'a> {
    r: &'a mut OrientedResizer,
    count: usize,
}

impl OrientedReady<'_> {
    pub const fn count(&self) -> usize {
        self.count
    }

    /// Places the next ready row; `false` when there are no more.
    pub fn pop(&mut self, dst: &mut [u8], stride: usize) -> Result<bool> {
        self.r.pop_into(&mut self.count, dst, stride)
    }
}

pub struct OrientedFinished {
    r: OrientedResizer,
    count: usize,
}

impl OrientedFinished {
    pub const fn count(&self) -> usize {
        self.count
    }

    pub fn pop(&mut self, dst: &mut [u8], stride: usize) -> Result<bool> {
        self.r.pop_into(&mut self.count, dst, stride)
    }
}

/// Resizes a decoder's YCbCr planes as they are sampled, each on its own, and
/// turns them into the output format only at the end. Places rows like
/// `OrientedResizer`, into the whole destination.
pub struct PlanarResizer {
    layout: PlaneLayout,
    orientation: Orientation,
    format: ImageFormat,
    dw: usize,
    dh: usize,
    luma: Core,
    chroma: ChromaScaler,
    lumas: VecDeque<(usize, Buffer), Caps>,
    chromas: VecDeque<(usize, Buffer, Buffer), Caps>,
    spare: Vec<Buffer, Caps>,
    luma_made: usize,
    chroma_made: usize,
    luma_in: usize,
    chroma_in: usize,
    placed: usize,
    row: Buffer,
    caps: Option<Caps>,
}

// The SIMD colour conversion reads past the end of a row.
const ROW_SLACK: usize = 16;

impl PlanarResizer {
    /// `format` defaults to `Rgb888`.
    pub fn new(layout: PlaneLayout, options: &ResizeOptions, orientation: Orientation) -> Result<Self> {
        let (dw16, dh16) = unoriented_size(layout.width, layout.height, options, orientation)?;
        let (dw, dh) = (usize::from(dw16), usize::from(dh16));
        let format = options.format.unwrap_or(ImageFormat::Rgb888);
        if !is_row_format(format) {
            return Err(Error::Unsupported);
        }
        let luma = ImageInfo { width: layout.width, height: layout.height, format: ImageFormat::L8 };
        let o = ResizeOptions { target_width: dw as u16, target_height: dh as u16, fit: Fit::Stretch, format: Some(ImageFormat::L8), caps: options.caps };
        let size = (usize::from(layout.width), usize::from(layout.height));
        let caps = options.caps.unwrap_or(Caps::DEFAULT);
        Ok(Self {
            layout,
            orientation,
            format,
            dw,
            dh,
            luma: Core::new(luma, &o, Orientation::None)?,
            chroma: ChromaScaler::new(size, layout.shift, (dw, dh), options.caps)?,
            lumas: VecDeque::new_in(caps),
            chromas: VecDeque::new_in(caps),
            spare: Vec::new_in(caps),
            luma_made: 0,
            chroma_made: 0,
            luma_in: 0,
            chroma_in: 0,
            placed: 0,
            row: buffer(if orientation == Orientation::None { 0 } else { format.row_bytes(dw) }, options.caps)?,
            caps: options.caps,
        })
    }

    /// The size after orientation.
    pub fn info(&self) -> ImageInfo {
        let (w, h) = (self.dw as u16, self.dh as u16);
        let (width, height) = if self.orientation.swaps_axes() { (h, w) } else { (w, h) };
        ImageInfo { width, height, format: self.format }
    }

    fn check(&self, dst: &[u8], stride: usize) -> Result<()> {
        let info = self.info();
        let row_bytes = info.row_bytes();
        if stride < row_bytes || dst.len() < (usize::from(info.height) - 1) * stride + row_bytes {
            return Err(Error::InvalidArgument);
        }
        Ok(())
    }

    fn take(&mut self) -> Result<Buffer> {
        match self.spare.pop() {
            Some(b) => Ok(b),
            None => buffer(self.dw + ROW_SLACK, self.caps),
        }
    }

    fn push_luma(&mut self, row: &[u8]) -> Result<()> {
        let mut ready = self.luma.push_row(row)?;
        while ready > 0 {
            ready -= 1;
            let mut b = self.take()?;
            if !self.luma.pop(&mut b) {
                self.spare.push(b);
                break;
            }
            self.lumas.push_back((self.luma_made, b));
            self.luma_made += 1;
        }
        self.luma_in += 1;
        Ok(())
    }

    fn push_chroma(&mut self, cb: &[u8], cr: &[u8]) -> Result<()> {
        let (spare, chromas, made, caps, w) = (&mut self.spare, &mut self.chromas, &mut self.chroma_made, self.caps, self.dw + ROW_SLACK);
        self.chroma.push([cb, cr], |cb, cr| {
            let mut pair = [spare.pop(), spare.pop()];
            for (b, row) in pair.iter_mut().zip([cb, cr]) {
                let b = match b {
                    Some(b) => b,
                    None => b.insert(buffer(w, caps)?),
                };
                b[..row.len()].copy_from_slice(row);
            }
            let [Some(b), Some(r)] = pair else { unreachable!() };
            chromas.push_back((*made, b, r));
            *made += 1;
            Ok(())
        })?;
        self.chroma_in += 1;
        Ok(())
    }

    fn drain(&mut self, dst: &mut [u8], stride: usize) -> usize {
        let mut placed = 0;
        while let Some(&(y, _)) = self.lumas.front() {
            let Some((k, cb, cr)) = self.chromas.front() else { break };
            if *k != y {
                break;
            }
            let (_, luma) = self.lumas.front().unwrap();
            let (shift, ycc, format, w) = ((0, 0), self.layout.ycc, self.format, self.dw);
            if self.orientation == Orientation::None {
                store_ycc_row(luma, cb, cr, shift, ycc, &mut dst[y * stride..], format, w);
            } else {
                store_ycc_row(luma, cb, cr, shift, ycc, &mut self.row, format, w);
                let bpp = format.bits() / 8;
                unsafe { place_row(self.orientation, self.dw, self.dh, bpp, &self.row, y, dst.as_mut_ptr(), stride) };
            }
            let (_, luma) = self.lumas.pop_front().unwrap();
            let (_, b, r) = self.chromas.pop_front().unwrap();
            self.spare.extend([luma, b, r]);
            placed += 1;
        }
        self.placed += placed;
        placed
    }

    /// Takes the planes a decoder finished, in order, and places the rows they
    /// complete. Returns how many.
    pub fn push(&mut self, planes: &Planes, dst: &mut [u8], stride: usize) -> Result<usize> {
        self.check(dst, stride)?;
        let [y, cb, cr] = planes.planes;
        if y.first != self.luma_in || cb.first != self.chroma_in || cr.first != cb.first || cr.count != cb.count {
            return Err(Error::InvalidArgument);
        }
        let vs = self.layout.shift.1;
        let mut placed = 0;
        let mut next = y.first;
        for c in cb.first..cb.end() {
            while next < y.end() && next < (c + 1) << vs {
                self.push_luma(y.row(next))?;
                next += 1;
                placed += self.drain(dst, stride);
            }
            self.push_chroma(cb.row(c), cr.row(c))?;
            placed += self.drain(dst, stride);
        }
        while next < y.end() {
            self.push_luma(y.row(next))?;
            next += 1;
            placed += self.drain(dst, stride);
        }
        Ok(placed)
    }

    /// Ends the input and places the last rows. Returns how many, or `Decode`
    /// when the planes stopped short of the picture.
    pub fn finish(mut self, dst: &mut [u8], stride: usize) -> Result<usize> {
        self.check(dst, stride)?;
        let ready = self.luma.finish();
        for _ in 0..ready {
            let mut b = self.take()?;
            if self.luma.pop(&mut b) {
                self.lumas.push_back((self.luma_made, b));
                self.luma_made += 1;
            }
        }
        let placed = self.drain(dst, stride);
        if self.placed != self.dh {
            return Err(Error::Decode);
        }
        Ok(placed)
    }
}

/// Every remaining row of an opened decoder into one new picture. Open the
/// decoder with a matching target so that it can downscale while it decodes.
pub fn resize_decoder(decoder: &mut Decoder, options: &ResizeOptions, orientation: Orientation) -> Result<ImageBuf<Caps>> {
    let info = decoder.info();
    let (width, height) = output_size(info.width, info.height, options, orientation)?;
    let format = options.format.unwrap_or(info.format);
    if !is_row_format(format) {
        return Err(Error::Unsupported);
    }
    let caps = options.caps.unwrap_or_else(default_caps);
    // Every row lands in it, or the call fails and it is dropped unread.
    let mut image = unsafe { ImageBuf::new_uninit_in(format, width, height, usize::from(width), 1, caps)? };
    let stride = image.stride_bytes();
    resize_decoder_into(decoder, image.pixels_mut(), stride, options, orientation)?;
    Ok(image)
}

/// `resize_decoder` into the caller's memory. A stride of 0 is tight packing.
/// A decoder with YCbCr planes is resized in them, so its chroma stays
/// subsampled until it is converted, once per output pixel.
pub fn resize_decoder_into(decoder: &mut Decoder, dst: &mut [u8], dst_stride: usize, options: &ResizeOptions, orientation: Orientation) -> Result<ImageInfo> {
    if let Some(layout) = decoder.plane_layout() {
        return resize_planes_into(decoder, layout, dst, dst_stride, options, orientation);
    }
    let info = decoder.info();
    let mut r = OrientedResizer::new(info, options, orientation)?;
    let out = r.info();
    let stride = if dst_stride == 0 { out.row_bytes() } else { dst_stride };
    if stride < out.row_bytes() || dst.len() < (usize::from(out.height) - 1) * stride + out.row_bytes() {
        return Err(Error::InvalidArgument);
    }
    let mut row: Buffer = buffer(info.row_bytes(), Some(Caps::DEFAULT))?;
    let rows = if orientation.swaps_axes() { usize::from(out.width) } else { usize::from(out.height) };
    let mut done = 0;
    for _ in 0..info.height {
        if !decoder.next_row(&mut row)? {
            return Err(Error::Truncated);
        }
        let mut ready = r.push_row(&row)?;
        while done < rows && ready.pop(dst, stride)? {
            done += 1;
        }
    }
    let mut last = r.finish();
    while done < rows && last.pop(dst, stride)? {
        done += 1;
    }
    if done != rows {
        return Err(Error::Decode);
    }
    Ok(out)
}

fn resize_planes_into(decoder: &mut Decoder, layout: PlaneLayout, dst: &mut [u8], dst_stride: usize, options: &ResizeOptions, orientation: Orientation) -> Result<ImageInfo> {
    let options = ResizeOptions { format: options.format.or(Some(decoder.info().format)), ..*options };
    let mut r = PlanarResizer::new(layout, &options, orientation)?;
    let out = r.info();
    let stride = if dst_stride == 0 { out.row_bytes() } else { dst_stride };
    while let Some(planes) = decoder.next_planes()? {
        r.push(&planes, dst, stride)?;
    }
    r.finish(dst, stride)?;
    Ok(out)
}

fn check_strides(info: ImageInfo, src_stride: usize, dst_format: ImageFormat, dst_w: u16, dst_stride: usize) -> Result<(usize, usize, usize)> {
    let src_stride = if src_stride == 0 { info.row_bytes() } else { src_stride };
    if src_stride < info.row_bytes() {
        return Err(Error::InvalidArgument);
    }
    let dst_row = dst_format.row_bytes(usize::from(dst_w));
    let dst_stride = if dst_stride == 0 { dst_row } else { dst_stride };
    if dst_stride < dst_row {
        return Err(Error::InvalidArgument);
    }
    Ok((src_stride, dst_stride, dst_row))
}

/// A whole picture from `src` to `dst`. A stride of 0 is tight packing.
pub fn resize_buffer(src: &[u8], info: ImageInfo, src_stride: usize, dst: &mut [u8], dst_stride: usize, options: &ResizeOptions, orientation: Orientation) -> Result<()> {
    let (dw, dh) = output_size(info.width, info.height, options, orientation)?;
    let format = options.format.unwrap_or(info.format);
    let (src_stride, dst_stride, dst_row) = check_strides(info, src_stride, format, dw, dst_stride)?;
    let last = usize::from(info.height) - 1;
    if src.len() < last * src_stride + info.row_bytes() || dst.len() < (usize::from(dh) - 1) * dst_stride + dst_row {
        return Err(Error::InvalidArgument);
    }
    let mut r = OrientedResizer::new(info, options, orientation)?;
    let rows = if orientation.swaps_axes() { usize::from(dw) } else { usize::from(dh) };
    let mut done = 0;
    for y in 0..=last {
        let mut ready = r.push_row(&src[y * src_stride..])?;
        while done < rows && ready.pop(dst, dst_stride)? {
            done += 1;
        }
    }
    let mut finished = r.finish();
    while done < rows && finished.pop(dst, dst_stride)? {
        done += 1;
    }
    if done == rows { Ok(()) } else { Err(Error::Decode) }
}

/// `resize_buffer` over one buffer, for a picture that shrinks on every axis, so
/// the output never overtakes the input. Anything else is `InvalidArgument`.
pub fn resize_in_place(buf: &mut [u8], info: ImageInfo, src_stride: usize, dst_stride: usize, options: &ResizeOptions) -> Result<()> {
    let (dw, dh) = output_size(info.width, info.height, options, Orientation::None)?;
    if dw > info.width || dh > info.height {
        return Err(Error::InvalidArgument);
    }
    let format = options.format.unwrap_or(info.format);
    let (src_stride, dst_stride, dst_row) = check_strides(info, src_stride, format, dw, dst_stride)?;
    let last = usize::from(info.height) - 1;
    if buf.len() < last * src_stride + info.row_bytes() || buf.len() < (usize::from(dh) - 1) * dst_stride + dst_row {
        return Err(Error::InvalidArgument);
    }
    let mut r = Resizer::new(info, options)?;
    let mut done = 0;
    for y in 0..=last {
        let mut ready = r.push_row(&buf[y * src_stride..y * src_stride + info.row_bytes()])?;
        while ready.pop(&mut buf[done * dst_stride..done * dst_stride + dst_row])? {
            done += 1;
        }
    }
    let mut finished = r.finish();
    while finished.pop(&mut buf[done * dst_stride..done * dst_stride + dst_row])? {
        done += 1;
    }
    if done == usize::from(dh) { Ok(()) } else { Err(Error::Decode) }
}

/// `resize_in_place` over a picture, which takes the new size, tightly packed.
pub fn resize_image_in_place<A: Allocator>(image: &mut ImageBuf<A>, options: &ResizeOptions) -> Result<()> {
    let info = ImageInfo { width: image.width(), height: image.height(), format: image.format() };
    let (width, height) = output_size(info.width, info.height, options, Orientation::None)?;
    let format = options.format.unwrap_or(info.format);
    let stride = image.stride_bytes();
    resize_in_place(image.pixels_mut(), info, stride, 0, options)?;
    image.reshape(format, width, height, usize::from(width));
    Ok(())
}
