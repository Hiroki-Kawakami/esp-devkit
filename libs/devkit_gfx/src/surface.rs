/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;
use core::alloc::Allocator;

use crate::color::{Color, mul8};
use crate::format::ImageFormat;
use crate::geom::Rect;
use crate::image::Image;
use crate::pixel::Pixel;

/// Writable pixels covering `area` in logical coordinates, `stride` bytes per
/// row. Everything drawn is clipped to `area`; the caller keeps the memory
/// valid and unshared while drawing.
#[derive(Clone, Copy)]
pub struct Surface {
    ptr: *mut u8,
    stride: usize,
    area: Rect,
    shift: u8,
}

unsafe impl Send for Surface {}

impl Surface {
    /// `ptr` must hold `area.h` rows of `stride` bytes.
    pub unsafe fn new(ptr: *mut u8, stride: usize, area: Rect) -> Self {
        Self { ptr, stride, area, shift: 0 }
    }

    /// `area` at half size: pixel (u, v) is the logical pixel at
    /// (`area.x + 2u + 1`, `area.y + 2v + 1`). `ptr` must hold `area.h / 2` rows.
    pub unsafe fn half(ptr: *mut u8, stride: usize, area: Rect) -> Self {
        Self { ptr, stride, area, shift: 1 }
    }

    pub fn area(&self) -> Rect {
        self.area
    }

    pub(crate) fn shift(&self) -> u8 {
        self.shift
    }

    /// The pixels in memory that `r`, inside `area`, covers.
    pub(crate) fn cells(&self, r: &Rect) -> Rect {
        let (x, y) = ((r.x - self.area.x) >> self.shift, (r.y - self.area.y) >> self.shift);
        let (right, bottom) = ((r.right() - self.area.x) >> self.shift, (r.bottom() - self.area.y) >> self.shift);
        Rect::new(x, y, right - x, bottom - y)
    }

    pub(crate) fn logical_x(&self, u: i16) -> i16 {
        self.area.x + (u << self.shift) + i16::from(self.shift)
    }

    pub(crate) fn logical_y(&self, v: i16) -> i16 {
        self.area.y + (v << self.shift) + i16::from(self.shift)
    }

    pub(crate) fn at<P: Pixel>(&self, u: i16, v: i16) -> *mut u8 {
        unsafe { self.ptr.add(v as usize * self.stride + u as usize * P::BYTES) }
    }

    pub(crate) fn bytes<P: Pixel>(&self, x: i16, y: i16, pixels: usize) -> *mut [u8] {
        core::ptr::slice_from_raw_parts_mut(self.row::<P>(x, y), pixels * P::BYTES)
    }

    fn row<P: Pixel>(&self, x: i16, y: i16) -> *mut u8 {
        let offset = (y - self.area.y) as usize * self.stride + (x - self.area.x) as usize * P::BYTES;
        unsafe { self.ptr.add(offset) }
    }

    pub(crate) fn fill<P: Pixel>(&self, rect: &Rect, clip: &Rect, c: Color) {
        let cells = self.cells(&rect.intersect(clip));
        if cells.is_empty() || c.a == 0 {
            return;
        }
        let packed = P::pack(c);
        let mut row = self.at::<P>(cells.x, cells.y);
        for _ in 0..cells.h {
            unsafe {
                if c.a == 255 {
                    P::fill(row, cells.w as usize, packed);
                } else {
                    P::blend(row, cells.w as usize, packed, c.a);
                }
                row = row.add(self.stride);
            }
        }
    }

    /// `mask` addresses `rect`'s top left pixel, and `mask_stride` may be
    /// negative or zero to walk rows backwards or repeat one.
    pub(crate) fn mask<P: Pixel>(&self, rect: &Rect, clip: &Rect, mask: *const u8, mask_stride: isize, c: Color) {
        let cells = self.cells(&rect.intersect(clip));
        if cells.is_empty() || c.a == 0 {
            return;
        }
        let packed = P::pack(c);
        let mut row = self.at::<P>(cells.x, cells.y);
        let (x, y) = (self.logical_x(cells.x), self.logical_y(cells.y));
        let skip = (y - rect.y) as isize * mask_stride + (x - rect.x) as isize;
        let mut m = unsafe { mask.offset(skip) };
        for _ in 0..cells.h {
            unsafe {
                if self.shift == 0 {
                    blend_mask_row::<P>(row, m, cells.w as usize, packed, c.a);
                } else {
                    let mut picked = [0u8; PICKED];
                    let mut done = 0;
                    while done < cells.w as usize {
                        let n = (cells.w as usize - done).min(PICKED);
                        for (i, slot) in picked[..n].iter_mut().enumerate() {
                            *slot = m.add((done + i) << self.shift).read();
                        }
                        blend_mask_row::<P>(row.add(done * P::BYTES), picked.as_ptr(), n, packed, c.a);
                        done += n;
                    }
                }
                row = row.add(self.stride);
                m = m.offset(mask_stride << self.shift);
            }
        }
    }

    /// Appends the pixels of `rect`, which must be inside the surface.
    pub(crate) fn save<P: Pixel, A: Allocator>(&self, rect: &Rect, out: &mut Vec<u8, A>) {
        let cells = self.cells(rect);
        let mut row = self.at::<P>(cells.x, cells.y);
        for _ in 0..cells.h {
            out.extend_from_slice(unsafe { core::slice::from_raw_parts(row, cells.w as usize * P::BYTES) });
            row = unsafe { row.add(self.stride) };
        }
    }

    /// Brings `saved`, rows of `rect` as `save` left them, back where `mask` is
    /// not full. `mask` addresses `rect`'s top left pixel.
    pub(crate) fn restore<P: Pixel>(&self, rect: &Rect, saved: *const u8, mask: *const u8, mask_stride: isize) {
        let cells = self.cells(rect);
        if cells.is_empty() {
            return;
        }
        let skip = (self.logical_y(cells.y) - rect.y) as isize * mask_stride + (self.logical_x(cells.x) - rect.x) as isize;
        let (width, mut row, mut from, mut m) = (cells.w as usize, self.at::<P>(cells.x, cells.y), saved, unsafe { mask.offset(skip) });
        for _ in 0..cells.h {
            for i in 0..width {
                unsafe {
                    let back = 255 - m.add(i << self.shift).read();
                    let (dst, src) = (row.add(i * P::BYTES), from.add(i * P::BYTES));
                    match back {
                        0 => {}
                        255 => core::ptr::copy_nonoverlapping(src, dst, P::BYTES),
                        _ => P::blend_mask(dst, &back, 1, P::load(src)),
                    }
                }
            }
            unsafe {
                row = row.add(self.stride);
                from = from.add(width * P::BYTES);
                m = m.offset(mask_stride << self.shift);
            }
        }
    }

    pub(crate) fn image<P: Pixel>(&self, x: i16, y: i16, image: &Image<'_>, clip: &Rect, tint: Color) {
        let rect = Rect::new(x, y, image.width() as i16, image.height() as i16);
        let r = rect.intersect(clip);
        if r.is_empty() {
            return;
        }
        let format = image.format();
        let src_stride = image.stride();
        let pixels = image.pixels();
        if format == ImageFormat::A8 {
            let start = (r.y - y) as usize * src_stride + (r.x - x) as usize;
            let mask = unsafe { pixels.as_ptr().add(start) };
            self.mask::<P>(&r, &r, mask, src_stride as isize, tint);
            return;
        }
        let cells = self.cells(&r);
        if cells.is_empty() {
            return;
        }
        let start = (self.logical_y(cells.y) - y) as usize * src_stride + (self.logical_x(cells.x) - x) as usize * format.bits() / 8;
        let step = format.bits() / 8 << self.shift;
        let mut dst = self.at::<P>(cells.x, cells.y);
        let mut src = unsafe { pixels.as_ptr().add(start) };
        for _ in 0..cells.h {
            unsafe {
                match format == P::NATIVE && tint.a == 255 {
                    true if self.shift == 0 => core::ptr::copy_nonoverlapping(src, dst, cells.w as usize * P::BYTES),
                    true => {
                        for i in 0..cells.w as usize {
                            core::ptr::copy_nonoverlapping(src.add(i * step), dst.add(i * P::BYTES), P::BYTES);
                        }
                    }
                    false => convert_row::<P>(dst, src, cells.w as usize, step, format, tint.a),
                }
                dst = dst.add(self.stride);
                src = src.add(src_stride << self.shift);
            }
        }
    }
}

const PICKED: usize = 64;

unsafe fn blend_mask_row<P: Pixel>(row: *mut u8, mask: *const u8, n: usize, c: P, alpha: u8) {
    unsafe {
        if alpha == 255 {
            P::blend_mask(row, mask, n, c);
        } else {
            P::blend_mask_alpha(row, mask, n, c, alpha);
        }
    }
}

unsafe fn convert_row<P: Pixel>(dst: *mut u8, src: *const u8, n: usize, step: usize, format: ImageFormat, opacity: u8) {
    for i in 0..n {
        let c = unsafe {
            let p = src.add(i * step);
            match format {
                ImageFormat::Rgb565 => {
                    let v = u16::from_le_bytes([p.read(), p.add(1).read()]);
                    let r = (v >> 11) as u8;
                    let g = (v >> 5) as u8 & 0x3f;
                    let b = v as u8 & 0x1f;
                    Color::rgb(r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2)
                }
                ImageFormat::Bgr888 => Color::rgb(p.add(2).read(), p.add(1).read(), p.read()),
                ImageFormat::Argb8888 => Color::rgba(p.add(2).read(), p.add(1).read(), p.read(), p.add(3).read()),
                ImageFormat::L8 => Color::rgb(p.read(), p.read(), p.read()),
                ImageFormat::A8 => Color::rgba(0, 0, 0, p.read()),
                _ => unreachable!(),
            }
        };
        let alpha = mul8(c.a, opacity);
        let d = unsafe { dst.add(i * P::BYTES) };
        unsafe {
            match alpha {
                0 => {}
                255 => P::fill(d, 1, P::pack(c)),
                a => P::blend(d, 1, P::pack(c), a),
            }
        }
    }
}
