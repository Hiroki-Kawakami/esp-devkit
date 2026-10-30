/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ptr;

use crate::color::{Color, mul8};
use crate::format::ImageFormat;

/// A framebuffer pixel format. `dst` and `mask` hold `n` pixels, `dst` aligned
/// for the format's unit.
pub trait Pixel: Copy + Send + Sync + 'static {
    const BYTES: usize;
    /// The image format copied without conversion.
    const NATIVE: ImageFormat;

    fn pack(c: Color) -> Self;

    unsafe fn fill(dst: *mut u8, n: usize, c: Self);

    /// `alpha` over the pixels.
    unsafe fn blend(dst: *mut u8, n: usize, c: Self, alpha: u8);

    /// The color through `mask`.
    unsafe fn blend_mask(dst: *mut u8, mask: *const u8, n: usize, c: Self);

    /// The color through `mask`, scaled by `alpha`.
    unsafe fn blend_mask_alpha(dst: *mut u8, mask: *const u8, n: usize, c: Self, alpha: u8);
}

#[derive(Clone, Copy)]
pub struct Rgb565(u16);

const SPREAD: u32 = 0x07E0_F81F;
// Half of a 5 bit alpha step in each field; without it layers darken by a level
// each, as the shift floors negative differences.
const SPREAD_ROUND: u32 = 0x0200_8010;

#[inline(always)]
const fn spread(v: u16) -> u32 {
    (v as u32 | (v as u32) << 16) & SPREAD
}

#[inline(always)]
fn blend565(d: *mut u16, src: u32, a5: u32) {
    let spreaded = spread(unsafe { d.read() });
    let difference = src.wrapping_sub(spreaded).wrapping_mul(a5).wrapping_add(SPREAD_ROUND);
    let out = spreaded.wrapping_add(difference >> 5) & SPREAD;
    unsafe { d.write((out | out >> 16) as u16) };
}

impl Pixel for Rgb565 {
    const BYTES: usize = 2;
    const NATIVE: ImageFormat = ImageFormat::Rgb565;

    #[inline]
    fn pack(c: Color) -> Self {
        let r = (c.r as u32 * 249 + 1014) >> 11;
        let g = (c.g as u32 * 253 + 505) >> 10;
        let b = (c.b as u32 * 249 + 1014) >> 11;
        Self((r << 11 | g << 5 | b) as u16)
    }

    #[inline]
    unsafe fn fill(dst: *mut u8, n: usize, c: Self) {
        let [lo, hi] = c.0.to_le_bytes();
        if lo == hi {
            unsafe { ptr::write_bytes(dst, lo, n * 2) };
            return;
        }
        let dst = dst.cast::<u16>();
        for i in 0..n {
            unsafe { dst.add(i).write(c.0) };
        }
    }

    #[inline]
    unsafe fn blend(dst: *mut u8, n: usize, c: Self, alpha: u8) {
        let dst = dst.cast::<u16>();
        let src = spread(c.0);
        let a5 = (u32::from(alpha) + 4) >> 3;
        for i in 0..n {
            blend565(unsafe { dst.add(i) }, src, a5);
        }
    }

    #[inline]
    unsafe fn blend_mask(dst: *mut u8, mask: *const u8, n: usize, c: Self) {
        let dst = dst.cast::<u16>();
        let src = spread(c.0);
        for i in 0..n {
            let a = u32::from(unsafe { *mask.add(i) });
            if a == 0 {
                continue;
            }
            let d = unsafe { dst.add(i) };
            if a >= 252 {
                unsafe { d.write(c.0) };
            } else {
                blend565(d, src, (a + 4) >> 3);
            }
        }
    }

    #[inline]
    unsafe fn blend_mask_alpha(dst: *mut u8, mask: *const u8, n: usize, c: Self, alpha: u8) {
        let dst = dst.cast::<u16>();
        let src = spread(c.0);
        for i in 0..n {
            let a = mul8(unsafe { *mask.add(i) }, alpha);
            if a != 0 {
                blend565(unsafe { dst.add(i) }, src, (u32::from(a) + 4) >> 3);
            }
        }
    }
}

#[derive(Clone, Copy)]
pub struct Bgr888([u8; 3]);

#[inline(always)]
fn blend888(d: *mut u8, src: [u8; 3], a: u32) {
    let a = a + (a >> 7);
    for (ch, &s) in src.iter().enumerate() {
        let p = unsafe { d.add(ch) };
        let v = i32::from(unsafe { p.read() });
        unsafe { p.write((v + ((i32::from(s) - v) * a as i32 >> 8)) as u8) };
    }
}

impl Pixel for Bgr888 {
    const BYTES: usize = 3;
    const NATIVE: ImageFormat = ImageFormat::Rgb888;

    #[inline]
    fn pack(c: Color) -> Self {
        Self([c.b, c.g, c.r])
    }

    #[inline]
    unsafe fn fill(dst: *mut u8, n: usize, c: Self) {
        let [b, g, r] = c.0;
        if b == g && g == r {
            unsafe { ptr::write_bytes(dst, b, n * 3) };
            return;
        }
        for i in 0..n {
            unsafe {
                dst.add(i * 3).write(b);
                dst.add(i * 3 + 1).write(g);
                dst.add(i * 3 + 2).write(r);
            }
        }
    }

    #[inline]
    unsafe fn blend(dst: *mut u8, n: usize, c: Self, alpha: u8) {
        for i in 0..n {
            blend888(unsafe { dst.add(i * 3) }, c.0, u32::from(alpha));
        }
    }

    #[inline]
    unsafe fn blend_mask(dst: *mut u8, mask: *const u8, n: usize, c: Self) {
        for i in 0..n {
            let a = u32::from(unsafe { *mask.add(i) });
            if a == 0 {
                continue;
            }
            let d = unsafe { dst.add(i * 3) };
            if a >= 252 {
                unsafe {
                    d.write(c.0[0]);
                    d.add(1).write(c.0[1]);
                    d.add(2).write(c.0[2]);
                }
            } else {
                blend888(d, c.0, a);
            }
        }
    }

    #[inline]
    unsafe fn blend_mask_alpha(dst: *mut u8, mask: *const u8, n: usize, c: Self, alpha: u8) {
        for i in 0..n {
            let a = mul8(unsafe { *mask.add(i) }, alpha);
            if a != 0 {
                blend888(unsafe { dst.add(i * 3) }, c.0, u32::from(a));
            }
        }
    }
}
