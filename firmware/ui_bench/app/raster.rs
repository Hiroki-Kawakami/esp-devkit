/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ptr;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let r = (self.x + self.w).max(other.x + other.w);
        let b = (self.y + self.h).max(other.y + other.h);
        Rect::new(x, y, r - x, b - y)
    }

    pub fn intersect(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = (self.x + self.w).min(other.x + other.w);
        let b = (self.y + self.h).min(other.y + other.h);
        Rect::new(x, y, r - x, b - y)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Color(pub u8, pub u8, pub u8);

pub trait Pixel: Copy {
    const BYTES: usize;

    fn pack(c: Color) -> Self;

    /// `dst` holds `n` pixels.
    unsafe fn fill(dst: *mut u8, n: usize, c: Self);

    /// `dst` and `mask` hold `n` pixels.
    unsafe fn blend_mask(dst: *mut u8, mask: *const u8, n: usize, c: Self);
}

#[derive(Clone, Copy)]
pub struct Rgb565(u16);

impl Pixel for Rgb565 {
    const BYTES: usize = 2;

    fn pack(Color(r, g, b): Color) -> Self {
        Self((u16::from(r) >> 3) << 11 | (u16::from(g) >> 2) << 5 | u16::from(b) >> 3)
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
    unsafe fn blend_mask(dst: *mut u8, mask: *const u8, n: usize, c: Self) {
        const SPREAD: u32 = 0x07E0_F81F;
        let dst = dst.cast::<u16>();
        let src = (u32::from(c.0) | u32::from(c.0) << 16) & SPREAD;
        for i in 0..n {
            let a = u32::from(unsafe { *mask.add(i) });
            if a == 0 {
                continue;
            }
            let d = unsafe { dst.add(i) };
            if a >= 252 {
                unsafe { d.write(c.0) };
                continue;
            }
            let v = u32::from(unsafe { d.read() });
            let spread = (v | v << 16) & SPREAD;
            let a = (a + 4) >> 3;
            let out = spread.wrapping_add(src.wrapping_sub(spread).wrapping_mul(a) >> 5) & SPREAD;
            unsafe { d.write((out | out >> 16) as u16) };
        }
    }
}

#[derive(Clone, Copy)]
pub struct Bgr888([u8; 3]);

impl Pixel for Bgr888 {
    const BYTES: usize = 3;

    fn pack(Color(r, g, b): Color) -> Self {
        Self([b, g, r])
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
    unsafe fn blend_mask(dst: *mut u8, mask: *const u8, n: usize, c: Self) {
        let src = c.0.map(i32::from);
        for i in 0..n {
            let a = i32::from(unsafe { *mask.add(i) });
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
                continue;
            }
            let a = a + (a >> 7);
            for (ch, s) in src.iter().enumerate() {
                let p = unsafe { d.add(ch) };
                let v = i32::from(unsafe { p.read() });
                unsafe { p.write((v + (((s - v) * a) >> 8)) as u8) };
            }
        }
    }
}

/// `w` x `h` pixels at logical (`x`, `y`), packed without row padding.
#[derive(Clone, Copy)]
pub struct Surface {
    pub ptr: *mut u8,
    pub area: Rect,
}

impl Surface {
    pub fn fill<P: Pixel>(&self, rect: &Rect, c: Color) {
        let r = rect.intersect(&self.area);
        if r.is_empty() {
            return;
        }
        let c = P::pack(c);
        let stride = self.area.w as usize * P::BYTES;
        let mut row = unsafe {
            self.ptr.add((r.y - self.area.y) as usize * stride + (r.x - self.area.x) as usize * P::BYTES)
        };
        for _ in 0..r.h {
            unsafe {
                P::fill(row, r.w as usize, c);
                row = row.add(stride);
            }
        }
    }

    pub fn mask<P: Pixel>(&self, rect: &Rect, mask: *const u8, c: Color) {
        let r = rect.intersect(&self.area);
        if r.is_empty() {
            return;
        }
        let c = P::pack(c);
        let stride = self.area.w as usize * P::BYTES;
        let mut row = unsafe {
            self.ptr.add((r.y - self.area.y) as usize * stride + (r.x - self.area.x) as usize * P::BYTES)
        };
        let mask_stride = rect.w as usize;
        let mut m = unsafe { mask.add((r.y - rect.y) as usize * mask_stride + (r.x - rect.x) as usize) };
        for _ in 0..r.h {
            unsafe {
                P::blend_mask(row, m, r.w as usize, c);
                row = row.add(stride);
                m = m.add(mask_stride);
            }
        }
    }
}
