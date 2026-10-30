/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ptr;

use idf::heap::Caps;

use crate::mem::{Buffer, buffer, uninit_buffer};
use crate::kernels;
use crate::{Decoder, Error, HAVE_PIE, ImageInfo, PixelFormat, Result};

const Q16: u32 = 65536;

// Rounding once per axis biases the result whenever both roundings land on a tie,
// which an integer ratio hits constantly, so a box vertical pass gets four
// fractional bits and does the only rounding.
const HROW_FRAC_BITS: u32 = 4;
const LUMA_R_Q8: i32 = 54;
const LUMA_G_Q8: i32 = 183;
const LUMA_B_Q8: i32 = 19;

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
    pub format: Option<PixelFormat>,
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

struct Bilinear {
    lo: Buffer<u32>,
    hi: Buffer<u32>,
    blend: Buffer<u32>,
}

impl Bilinear {
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
fn clamp8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

fn unpack_row(src: &[u8], dst: &mut [u16], w: usize, format: PixelFormat, ch: usize) {
    if ch == 1 {
        for (d, &s) in dst.iter_mut().zip(&src[..w]) {
            *d = u16::from(s);
        }
        return;
    }
    match format {
        PixelFormat::Gray8 => {
            for (d, &g) in dst.chunks_exact_mut(3).zip(&src[..w]) {
                d.fill(u16::from(g));
            }
        }
        PixelFormat::Rgb888 | PixelFormat::Bgr888 => {
            let r = if format == PixelFormat::Bgr888 { 2 } else { 0 };
            for (d, s) in dst.chunks_exact_mut(3).zip(src[..w * 3].chunks_exact(3)) {
                d[0] = u16::from(s[r]);
                d[1] = u16::from(s[1]);
                d[2] = u16::from(s[2 - r]);
            }
        }
        PixelFormat::Rgb565 => {
            for (d, s) in dst.chunks_exact_mut(3).zip(src[..w * 2].chunks_exact(2)) {
                let p = u16::from_ne_bytes([s[0], s[1]]);
                let (r5, g6, b5) = ((p >> 11) & 0x1F, (p >> 5) & 0x3F, p & 0x1F);
                d[0] = (r5 << 3) | (r5 >> 2);
                d[1] = (g6 << 2) | (g6 >> 4);
                d[2] = (b5 << 3) | (b5 >> 2);
            }
        }
    }
}

fn pack_row(src: &[u16], dst: &mut [u8], w: usize, format: PixelFormat, ch: usize) {
    if ch == 1 {
        let mut i = 0;
        if HAVE_PIE && format == PixelFormat::Gray8 && w >= 16 {
            let n = w >> 4;
            kernels::pack_bytes(dst, src, n);
            i = n << 4;
        }
        for (d, &s) in dst[i..w].iter_mut().zip(&src[i..]) {
            *d = clamp8(i32::from(s));
        }
        return;
    }
    match format {
        PixelFormat::Gray8 => {
            for (d, s) in dst[..w].iter_mut().zip(src.chunks_exact(3)) {
                let (r, g, b) = (i32::from(s[0]), i32::from(s[1]), i32::from(s[2]));
                *d = clamp8((r * LUMA_R_Q8 + g * LUMA_G_Q8 + b * LUMA_B_Q8 + 128) >> 8);
            }
        }
        PixelFormat::Rgb888 | PixelFormat::Bgr888 => {
            let r = if format == PixelFormat::Bgr888 { 2 } else { 0 };
            let mut i = 0;
            if HAVE_PIE && r == 0 && w * 3 >= 16 {
                let n = (w * 3) >> 4;
                kernels::pack_bytes(dst, src, n);
                i = (n << 4) / 3;
            }
            for (d, s) in dst[i * 3..w * 3].chunks_exact_mut(3).zip(src[i * 3..].chunks_exact(3)) {
                d[r] = clamp8(i32::from(s[0]));
                d[1] = clamp8(i32::from(s[1]));
                d[2 - r] = clamp8(i32::from(s[2]));
            }
        }
        PixelFormat::Rgb565 => {
            let mut i = 0;
            if HAVE_PIE && w >= 8 {
                let n = w >> 3;
                kernels::pack_rgb565(dst, src, n);
                i = n << 3;
            }
            for (d, s) in dst[i * 2..w * 2].chunks_exact_mut(2).zip(src[i * 3..].chunks_exact(3)) {
                let r = u16::from(clamp8(i32::from(s[0]))) >> 3;
                let g = u16::from(clamp8(i32::from(s[1]))) >> 2;
                let b = u16::from(clamp8(i32::from(s[2]))) >> 3;
                d.copy_from_slice(&((r << 11) | (g << 5) | b).to_ne_bytes());
            }
        }
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
    src_format: PixelFormat,
    dst_format: PixelFormat,
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
        let caps = options.caps;
        let ch = if src_format == PixelFormat::Gray8 && dst_format == PixelFormat::Gray8 { 1 } else { 3 };
        let src_direct = src_format == PixelFormat::Rgb888 && ch == 3 || src_format == PixelFormat::Gray8 && ch == 1;
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
                pending: buffer(dw * dst_format.bytes_per_pixel(), caps)?,
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
            src_bytes: sw * src_format.bytes_per_pixel(),
            dst_bytes: dw * dst_format.bytes_per_pixel(),
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
        let (w, h, bpp) = (self.core.dw as isize, self.core.dh as isize, self.core.dst_format.bytes_per_pixel() as isize);
        let (y, s) = (y as isize, stride as isize);
        let (at, step) = match self.orientation {
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
        for px in self.row.chunks_exact(bpp as usize) {
            unsafe {
                ptr::copy_nonoverlapping(px.as_ptr(), d, bpp as usize);
                d = d.offset(step);
            }
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

/// A resized picture of `resize_decoder`.
pub struct ResizedImage {
    pub data: Buffer,
    pub info: ImageInfo,
}

/// Every remaining row of an opened decoder into one new buffer. Open the decoder
/// with a matching target so that it can downscale while it decodes.
pub fn resize_decoder(decoder: &mut Decoder, options: &ResizeOptions, orientation: Orientation) -> Result<ResizedImage> {
    let info = decoder.info();
    let mut r = OrientedResizer::new(info, options, orientation)?;
    let out = r.info();
    let stride = out.row_bytes();
    // Every row lands in it, or the call fails and it is dropped unread.
    let mut data = unsafe { uninit_buffer(stride * usize::from(out.height), options.caps)? };
    let mut row: Buffer = buffer(info.row_bytes(), Some(Caps::DEFAULT))?;
    let rows = if orientation.swaps_axes() { usize::from(out.width) } else { usize::from(out.height) };
    let mut done = 0;
    for _ in 0..info.height {
        if !decoder.next_row(&mut row)? {
            return Err(Error::Truncated);
        }
        let mut ready = r.push_row(&row)?;
        while done < rows && ready.pop(&mut data, stride)? {
            done += 1;
        }
    }
    let mut last = r.finish();
    while done < rows && last.pop(&mut data, stride)? {
        done += 1;
    }
    if done != rows {
        return Err(Error::Decode);
    }
    Ok(ResizedImage { data, info: out })
}

fn check_strides(info: ImageInfo, src_stride: usize, dst_format: PixelFormat, dst_w: u16, dst_stride: usize) -> Result<(usize, usize, usize)> {
    let src_stride = if src_stride == 0 { info.row_bytes() } else { src_stride };
    if src_stride < info.row_bytes() {
        return Err(Error::InvalidArgument);
    }
    let dst_row = usize::from(dst_w) * dst_format.bytes_per_pixel();
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
