/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

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
}

unsafe impl Send for Surface {}

impl Surface {
    /// `ptr` must hold `area.h` rows of `stride` bytes.
    pub unsafe fn new(ptr: *mut u8, stride: usize, area: Rect) -> Self {
        Self { ptr, stride, area }
    }

    pub fn area(&self) -> Rect {
        self.area
    }

    fn row<P: Pixel>(&self, x: i16, y: i16) -> *mut u8 {
        let offset = (y - self.area.y) as usize * self.stride + (x - self.area.x) as usize * P::BYTES;
        unsafe { self.ptr.add(offset) }
    }

    pub(crate) fn fill<P: Pixel>(&self, rect: &Rect, clip: &Rect, c: Color) {
        let r = rect.intersect(clip);
        if r.is_empty() || c.a == 0 {
            return;
        }
        let packed = P::pack(c);
        let mut row = self.row::<P>(r.x, r.y);
        for _ in 0..r.h {
            unsafe {
                if c.a == 255 {
                    P::fill(row, r.w as usize, packed);
                } else {
                    P::blend(row, r.w as usize, packed, c.a);
                }
                row = row.add(self.stride);
            }
        }
    }

    /// `mask` addresses `rect`'s top left pixel, and `mask_stride` may be
    /// negative or zero to walk rows backwards or repeat one.
    pub(crate) fn mask<P: Pixel>(&self, rect: &Rect, clip: &Rect, mask: *const u8, mask_stride: isize, c: Color) {
        let r = rect.intersect(clip);
        if r.is_empty() || c.a == 0 {
            return;
        }
        let packed = P::pack(c);
        let mut row = self.row::<P>(r.x, r.y);
        let skip = (r.y - rect.y) as isize * mask_stride + (r.x - rect.x) as isize;
        let mut m = unsafe { mask.offset(skip) };
        for _ in 0..r.h {
            unsafe {
                if c.a == 255 {
                    P::blend_mask(row, m, r.w as usize, packed);
                } else {
                    P::blend_mask_alpha(row, m, r.w as usize, packed, c.a);
                }
                row = row.add(self.stride);
                m = m.offset(mask_stride);
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
        let start = (r.y - y) as usize * src_stride + (r.x - x) as usize * format.bytes();
        let pixels = image.pixels();
        if format == ImageFormat::A8 {
            let mask = unsafe { pixels.as_ptr().add(start) };
            let origin = Rect::new(r.x, r.y, r.w, r.h);
            self.mask::<P>(&origin, &origin, mask, src_stride as isize, tint);
            return;
        }
        let mut dst = self.row::<P>(r.x, r.y);
        let mut src = unsafe { pixels.as_ptr().add(start) };
        for _ in 0..r.h {
            unsafe {
                if format == P::NATIVE && tint.a == 255 {
                    core::ptr::copy_nonoverlapping(src, dst, r.w as usize * P::BYTES);
                } else {
                    convert_row::<P>(dst, src, r.w as usize, format, tint.a);
                }
                dst = dst.add(self.stride);
                src = src.add(src_stride);
            }
        }
    }
}

unsafe fn convert_row<P: Pixel>(dst: *mut u8, src: *const u8, n: usize, format: ImageFormat, opacity: u8) {
    for i in 0..n {
        let c = unsafe {
            let p = src.add(i * format.bytes());
            match format {
                ImageFormat::Rgb565 => {
                    let v = u16::from_le_bytes([p.read(), p.add(1).read()]);
                    let r = (v >> 11) as u8;
                    let g = (v >> 5) as u8 & 0x3f;
                    let b = v as u8 & 0x1f;
                    Color::rgb(r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2)
                }
                ImageFormat::Rgb888 => Color::rgb(p.add(2).read(), p.add(1).read(), p.read()),
                ImageFormat::Argb8888 => Color::rgba(p.add(2).read(), p.add(1).read(), p.read(), p.add(3).read()),
                ImageFormat::L8 => Color::rgb(p.read(), p.read(), p.read()),
                ImageFormat::A8 => Color::rgba(0, 0, 0, p.read()),
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
