/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::slice;

use crate::kernels;
use crate::{Error, HAVE_PIE, ImageFormat, Result, is_row_format};

const LUMA_R_Q8: i32 = 54;
const LUMA_G_Q8: i32 = 183;
const LUMA_B_Q8: i32 = 19;

#[inline(always)]
pub(crate) fn clamp8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

#[inline(always)]
fn luma(r: u8, g: u8, b: u8) -> u8 {
    clamp8((i32::from(r) * LUMA_R_Q8 + i32::from(g) * LUMA_G_Q8 + i32::from(b) * LUMA_B_Q8 + 128) >> 8)
}

pub(crate) fn out_format(native: ImageFormat, format: Option<ImageFormat>) -> Result<ImageFormat> {
    let format = format.unwrap_or(native);
    if is_row_format(format) { Ok(format) } else { Err(Error::Unsupported) }
}

pub(crate) trait Store {
    const BYTES: usize;
    fn rgb(out: &mut [u8], r: u8, g: u8, b: u8);
    fn gray(out: &mut [u8], l: u8);
}

pub(crate) struct L8;
pub(crate) struct Rgb888;
pub(crate) struct Bgr888;
pub(crate) struct Rgb565;

impl Store for L8 {
    const BYTES: usize = 1;

    #[inline(always)]
    fn rgb(out: &mut [u8], r: u8, g: u8, b: u8) {
        out[0] = luma(r, g, b);
    }

    #[inline(always)]
    fn gray(out: &mut [u8], l: u8) {
        out[0] = l;
    }
}

impl Store for Rgb888 {
    const BYTES: usize = 3;

    #[inline(always)]
    fn rgb(out: &mut [u8], r: u8, g: u8, b: u8) {
        out[..3].copy_from_slice(&[r, g, b]);
    }

    #[inline(always)]
    fn gray(out: &mut [u8], l: u8) {
        out[..3].fill(l);
    }
}

impl Store for Bgr888 {
    const BYTES: usize = 3;

    #[inline(always)]
    fn rgb(out: &mut [u8], r: u8, g: u8, b: u8) {
        out[..3].copy_from_slice(&[b, g, r]);
    }

    #[inline(always)]
    fn gray(out: &mut [u8], l: u8) {
        out[..3].fill(l);
    }
}

impl Store for Rgb565 {
    const BYTES: usize = 2;

    #[inline(always)]
    fn rgb(out: &mut [u8], r: u8, g: u8, b: u8) {
        let p = (u16::from(r) >> 3) << 11 | (u16::from(g) >> 2) << 5 | u16::from(b) >> 3;
        out[..2].copy_from_slice(&p.to_ne_bytes());
    }

    #[inline(always)]
    fn gray(out: &mut [u8], l: u8) {
        Self::rgb(out, l, l, l);
    }
}

/// Runs `$e` with `$S` naming the `Store` of a row format.
macro_rules! with_store {
    ($format:expr, $S:ident => $e:expr) => {
        match $format {
            $crate::ImageFormat::L8 => {
                type $S = $crate::pixel::L8;
                $e
            }
            $crate::ImageFormat::Rgb888 => {
                type $S = $crate::pixel::Rgb888;
                $e
            }
            $crate::ImageFormat::Bgr888 => {
                type $S = $crate::pixel::Bgr888;
                $e
            }
            $crate::ImageFormat::Rgb565 => {
                type $S = $crate::pixel::Rgb565;
                $e
            }
            _ => unreachable!(),
        }
    };
}
pub(crate) use with_store;

/// `w` pixels of `L8` or `Rgb888` into a row format.
pub(crate) fn store_row(src: &[u8], src_format: ImageFormat, dst: &mut [u8], format: ImageFormat, w: usize) {
    if src_format == format {
        let n = format.row_bytes(w);
        dst[..n].copy_from_slice(&src[..n]);
        return;
    }
    with_store!(format, S => {
        let out = dst[..w * S::BYTES].chunks_exact_mut(S::BYTES);
        if src_format == ImageFormat::L8 {
            for (o, &l) in out.zip(&src[..w]) {
                S::gray(o, l);
            }
        } else {
            for (o, p) in out.zip(src[..w * 3].chunks_exact(3)) {
                S::rgb(o, p[0], p[1], p[2]);
            }
        }
    })
}

/// How a YCbCr sample turns into RGB.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Ycc {
    /// JFIF, full range, Q8 with a single rounding so that the SIMD kernel
    /// matches it exactly.
    Jfif,
    /// VP8, studio range, in libwebp's arithmetic.
    Vp8,
}

#[inline(always)]
fn vp8_clip(v: i32) -> u8 {
    if v & !16383 == 0 {
        (v >> 6) as u8
    } else if v < 0 {
        0
    } else {
        255
    }
}

#[inline(always)]
pub(crate) fn vp8_rgb(y: i32, u: i32, v: i32) -> (u8, u8, u8) {
    let luma = (y * 19077) >> 8;
    (
        vp8_clip(luma + ((v * 26149) >> 8) - 14234),
        vp8_clip(luma - ((u * 6419) >> 8) - ((v * 13320) >> 8) + 8708),
        vp8_clip(luma + ((u * 33050) >> 8) - 17685),
    )
}

#[inline(always)]
fn jfif_rgb(y: u8, cb: u8, cr: u8) -> (u8, u8, u8) {
    let y0 = i32::from(y) << 8;
    let (cb, cr) = (i32::from(cb) - 128, i32::from(cr) - 128);
    (clamp8((y0 + 359 * cr) >> 8), clamp8((y0 - 88 * cb - 183 * cr) >> 8), clamp8((y0 + 454 * cb) >> 8))
}

/// One row of YCbCr samples into a row format, each chroma sample covering
/// `1 << shift` pixels across. `L8` is the luma alone. On a chip with PIE the
/// three rows must stay readable 16 bytes past their ends.
pub(crate) fn store_ycc_row(y: &[u8], cb: &[u8], cr: &[u8], shift: (u32, u32), ycc: Ycc, dst: &mut [u8], format: ImageFormat, w: usize) {
    if format == ImageFormat::L8 {
        match ycc {
            Ycc::Jfif => dst[..w].copy_from_slice(&y[..w]),
            Ycc::Vp8 => {
                for (o, &l) in dst[..w].iter_mut().zip(&y[..w]) {
                    *o = vp8_clip(((i32::from(l) * 19077) >> 8) - 1160);
                }
            }
        }
        return;
    }
    let (hs1, hs2) = shift;
    assert!(y.len() >= w && cb.len() >= w.div_ceil(1 << hs1) && cr.len() >= w.div_ceil(1 << hs2));
    let mut x = 0;
    if HAVE_PIE && ycc == Ycc::Jfif && hs1 == hs2 && hs1 <= 1 && w >> 4 > 0 {
        let n = w >> 4;
        unsafe { kernels::ycc_rgb(dst.as_mut_ptr(), y.as_ptr(), cb.as_ptr(), cr.as_ptr(), n, hs1 != 0, format) };
        x = n << 4;
    }
    with_store!(format, S => {
        for (i, o) in dst[x * S::BYTES..w * S::BYTES].chunks_exact_mut(S::BYTES).enumerate() {
            let px = x + i;
            let (l, u, v) = (y[px], cb[px >> hs1], cr[px >> hs2]);
            let (r, g, b) = match ycc {
                Ycc::Jfif => jfif_rgb(l, u, v),
                Ycc::Vp8 => vp8_rgb(i32::from(l), i32::from(u), i32::from(v)),
            };
            S::rgb(o, r, g, b);
        }
    })
}

/// A row in any row format into `ch` channels: 1 for `L8` to `L8`, else R, G, B.
pub(crate) fn unpack_row(src: &[u8], dst: &mut [u16], w: usize, format: ImageFormat, ch: usize) {
    if ch == 1 {
        for (d, &s) in dst.iter_mut().zip(&src[..w]) {
            *d = u16::from(s);
        }
        return;
    }
    match format {
        ImageFormat::L8 => {
            for (d, &g) in dst.chunks_exact_mut(3).zip(&src[..w]) {
                d.fill(u16::from(g));
            }
        }
        ImageFormat::Rgb888 | ImageFormat::Bgr888 => {
            let r = if format == ImageFormat::Bgr888 { 2 } else { 0 };
            for (d, s) in dst.chunks_exact_mut(3).zip(src[..w * 3].chunks_exact(3)) {
                d[0] = u16::from(s[r]);
                d[1] = u16::from(s[1]);
                d[2] = u16::from(s[2 - r]);
            }
        }
        ImageFormat::Rgb565 => {
            for (d, s) in dst.chunks_exact_mut(3).zip(src[..w * 2].chunks_exact(2)) {
                let p = u16::from_ne_bytes([s[0], s[1]]);
                let (r5, g6, b5) = ((p >> 11) & 0x1F, (p >> 5) & 0x3F, p & 0x1F);
                d[0] = (r5 << 3) | (r5 >> 2);
                d[1] = (g6 << 2) | (g6 >> 4);
                d[2] = (b5 << 3) | (b5 >> 2);
            }
        }
        _ => unreachable!(),
    }
}

/// `unpack_row` backwards, saturating each channel.
pub(crate) fn pack_row(src: &[u16], dst: &mut [u8], w: usize, format: ImageFormat, ch: usize) {
    if ch == 1 {
        let mut i = 0;
        if HAVE_PIE && format == ImageFormat::L8 && w >= 16 {
            let n = w >> 4;
            kernels::pack_bytes(dst, src, n);
            i = n << 4;
        }
        for (d, &s) in dst[i..w].iter_mut().zip(&src[i..]) {
            *d = clamp8(i32::from(s));
        }
        return;
    }
    let c = |s: u16| clamp8(i32::from(s));
    match format {
        ImageFormat::L8 => {
            for (d, s) in dst[..w].iter_mut().zip(src.chunks_exact(3)) {
                L8::rgb(slice::from_mut(d), c(s[0]), c(s[1]), c(s[2]));
            }
        }
        ImageFormat::Rgb888 | ImageFormat::Bgr888 => {
            let mut i = 0;
            if HAVE_PIE && format == ImageFormat::Rgb888 && w * 3 >= 16 {
                let n = (w * 3) >> 4;
                kernels::pack_bytes(dst, src, n);
                i = (n << 4) / 3;
            }
            with_store!(format, S => {
                for (d, s) in dst[i * 3..w * 3].chunks_exact_mut(3).zip(src[i * 3..].chunks_exact(3)) {
                    S::rgb(d, c(s[0]), c(s[1]), c(s[2]));
                }
            })
        }
        ImageFormat::Rgb565 => {
            let mut i = 0;
            if HAVE_PIE && w >= 8 {
                let n = w >> 3;
                kernels::pack_rgb565(dst, src, n);
                i = n << 3;
            }
            for (d, s) in dst[i * 2..w * 2].chunks_exact_mut(2).zip(src[i * 3..].chunks_exact(3)) {
                Rgb565::rgb(d, c(s[0]), c(s[1]), c(s[2]));
            }
        }
        _ => unreachable!(),
    }
}
