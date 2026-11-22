/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::format::ImageFormat;
use crate::geom::Rotation;
use crate::image::Image;
use crate::target::Target;

/// Puts `image` into the target's area, turned as the target says. The image
/// must be the area's size and in the target's format, or in one of `Rgb565`
/// and `Rgb565Be` for the other; `false` when it is not.
pub fn copy(image: &Image, target: &mut Target) -> bool {
    let swap = match (image.format(), target.format()) {
        (a, b) if a == b => false,
        (ImageFormat::Rgb565, ImageFormat::Rgb565Be) | (ImageFormat::Rgb565Be, ImageFormat::Rgb565) => true,
        _ => return false,
    };
    let area = target.area();
    let bits = target.format().bits();
    if bits % 8 != 0 || i32::from(image.width()) != i32::from(area.w) || i32::from(image.height()) != i32::from(area.h) {
        return false;
    }
    let bpp = bits / 8;
    let row = bpp * usize::from(image.width());
    let stride = target.stride_bytes() as isize;
    let (w, h) = (target.size().width as isize, target.size().height as isize);
    let (x, y, step) = (isize::from(area.x), isize::from(area.y), bpp as isize);
    let ((sx, sy), along, down) = match target.rotation() {
        Rotation::R0 => ((x, y), step, stride),
        Rotation::R90 => ((y, h - 1 - x), -stride, step),
        Rotation::R180 => ((w - 1 - x, h - 1 - y), -step, -stride),
        Rotation::R270 => ((w - 1 - y, x), stride, -step),
    };
    let base = target.as_ptr();
    let first = sy * stride + sx * step;
    if along == step {
        for j in 0..usize::from(image.height()) {
            let src = &image.pixels()[j * image.stride_bytes()..][..row];
            let dst = unsafe { core::slice::from_raw_parts_mut(base.offset(first + j as isize * down), row) };
            dst.copy_from_slice(src);
            if swap {
                swap_rgb565(dst);
            }
        }
        return true;
    }
    let walk = Walk {
        src: image.pixels().as_ptr(),
        src_stride: image.stride_bytes(),
        width: usize::from(image.width()),
        height: usize::from(image.height()),
        dst: unsafe { base.offset(first) },
        along,
        down,
    };
    // Xtensa faults on a misaligned 16-bit access rather than splitting it.
    let aligned = |align: usize| {
        (walk.src as usize | walk.dst as usize | walk.src_stride | stride as usize) % align == 0
    };
    unsafe {
        match (bpp, swap) {
            (1, _) => walk.run(|s: *const u8, d: *mut u8| *d = *s),
            (2, false) if aligned(2) => walk.run16::<false>(),
            (2, true) if aligned(2) => walk.run16::<true>(),
            (4, _) if aligned(4) => walk.run(|s: *const u32, d: *mut u32| *d = *s),
            (2, true) => walk.run(|s: *const [u8; 2], d: *mut [u8; 2]| *d = [(*s)[1], (*s)[0]]),
            (2, false) => walk.run(|s: *const [u8; 2], d: *mut [u8; 2]| *d = *s),
            (3, _) => walk.run(|s: *const [u8; 3], d: *mut [u8; 3]| *d = *s),
            (4, _) => walk.run(|s: *const [u8; 4], d: *mut [u8; 4]| *d = *s),
            _ => return false,
        }
    }
    true
}

/// An image walked row by row, each row laid down `along` bytes a pixel from
/// where the last began plus `down`.
struct Walk {
    src: *const u8,
    src_stride: usize,
    width: usize,
    height: usize,
    dst: *mut u8,
    along: isize,
    down: isize,
}

impl Walk {
    /// `put` moves one pixel of type `T`.
    #[inline(always)]
    unsafe fn run<T>(&self, put: impl Fn(*const T, *mut T)) {
        for j in 0..self.height {
            unsafe { self.row(j, &put) };
        }
    }

    #[inline(always)]
    unsafe fn row<T>(&self, j: usize, put: &impl Fn(*const T, *mut T)) {
        let src = unsafe { self.src.add(j * self.src_stride) }.cast::<T>();
        let mut dst = unsafe { self.dst.offset(j as isize * self.down) };
        for i in 0..self.width {
            put(unsafe { src.add(i) }, dst.cast::<T>());
            dst = dst.wrapping_offset(self.along);
        }
    }

    /// 16-bit pixels, the source and destination aligned to them. On a quarter
    /// turn two source rows fall side by side, so a pair goes out as one word.
    unsafe fn run16<const SWAP: bool>(&self) {
        let put = |s: *const u16, d: *mut u16| unsafe { *d = if SWAP { (*s).swap_bytes() } else { *s } };
        let pairs = self.down.unsigned_abs() == 2 && (self.along as usize) % 4 == 0;
        let mut j = 0;
        while pairs && j + 1 < self.height {
            let first = unsafe { self.dst.offset(j as isize * self.down) };
            // The pair's lower address holds the row that comes first in memory.
            let (low, high, at) = if self.down > 0 { (j, j + 1, first) } else { (j + 1, j, first.wrapping_offset(-2)) };
            if at as usize % 4 != 0 {
                break;
            }
            let (low, high) = unsafe {
                (self.src.add(low * self.src_stride).cast::<u16>(), self.src.add(high * self.src_stride).cast::<u16>())
            };
            let mut at = at.cast::<u32>();
            for i in 0..self.width {
                let word = unsafe { u32::from(*low.add(i)) | u32::from(*high.add(i)) << 16 };
                let word = if SWAP { (word & 0x00FF_00FF) << 8 | (word >> 8) & 0x00FF_00FF } else { word };
                unsafe { *at = word };
                at = at.wrapping_byte_offset(self.along);
            }
            j += 2;
        }
        while j < self.height {
            unsafe { self.row(j, &put) };
            j += 1;
        }
    }
}

/// Packs rows of `width` `L8` pixels, back to back in `pixels`, into `L4` rows
/// in place, each starting on a byte, and returns the bytes they take.
pub fn pack_l4(pixels: &mut [u8], width: usize) -> usize {
    if width == 0 {
        return 0;
    }
    let row_bytes = width.div_ceil(2);
    let rows = pixels.len() / width;
    let p = pixels.as_mut_ptr();
    for r in 0..rows {
        // In place: each byte lands before the pixels it takes.
        unsafe {
            let (src, dst) = (p.add(r * width), p.add(r * row_bytes));
            for i in 0..width / 2 {
                let pair = src.add(2 * i);
                dst.add(i).write((pair.read() & 0xF0) | (pair.add(1).read() >> 4));
            }
            if width % 2 == 1 {
                dst.add(width / 2).write(src.add(width - 1).read() & 0xF0);
            }
        }
    }
    rows * row_bytes
}

/// [`pack_l4`] for `L1`, white from half up.
pub fn pack_l1(pixels: &mut [u8], width: usize) -> usize {
    if width == 0 {
        return 0;
    }
    let row_bytes = width.div_ceil(8);
    let rows = pixels.len() / width;
    // Each byte goes no later than the first pixel it takes.
    for r in 0..rows {
        for i in 0..row_bytes {
            let mut byte = 0;
            for k in 0..8 {
                let x = i * 8 + k;
                if x < width {
                    byte |= (pixels[r * width + x] >> 7) << (7 - k);
                }
            }
            pixels[r * row_bytes + i] = byte;
        }
    }
    rows * row_bytes
}

/// Turns `Rgb565` pixels into `Rgb565Be` in place, and back.
pub fn swap_rgb565(pixels: &mut [u8]) {
    let done = unsafe { crate::simd::swap565(pixels.as_mut_ptr(), pixels.len() / 2) };
    let pixels = &mut pixels[done * 2..];
    let (head, words, tail) = unsafe { pixels.align_to_mut::<u32>() };
    if head.len() % 2 != 0 {
        pixels.chunks_exact_mut(2).for_each(|p| p.swap(0, 1));
        return;
    }
    head.chunks_exact_mut(2).for_each(|p| p.swap(0, 1));
    for v in words {
        *v = (*v & 0x00FF_00FF) << 8 | (*v >> 8) & 0x00FF_00FF;
    }
    tail.chunks_exact_mut(2).for_each(|p| p.swap(0, 1));
}
