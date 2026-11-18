/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ptr;

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
    for j in 0..usize::from(image.height()) {
        let src = &image.pixels()[j * image.stride_bytes()..][..row];
        let mut at = sy * stride + sx * step + j as isize * down;
        if along == step && !swap {
            unsafe { ptr::copy_nonoverlapping(src.as_ptr(), base.offset(at), row) };
            continue;
        }
        for px in src.chunks_exact(bpp) {
            unsafe {
                let dst = base.offset(at);
                if swap {
                    *dst = px[1];
                    *dst.add(1) = px[0];
                } else {
                    ptr::copy_nonoverlapping(px.as_ptr(), dst, bpp);
                }
            }
            at += along;
        }
    }
    true
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
