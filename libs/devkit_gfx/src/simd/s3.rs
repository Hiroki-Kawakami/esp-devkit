/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! ESP32-S3 PIE, in s3.S. The kernels take whole groups of pixels from a
//! 16-byte aligned output, so each call first does the pixels before that
//! itself, and returns how many it did in all; the caller does the rest.
//! Without PIE in the build they do none.

use super::{UNIT, Unit, mix565};

unsafe extern "C" {
    fn gfx_s3_blend565(out: *mut u16, back: *const u16, front: *const u16, n8: i32, a5: i32, consts: *const u16);
    fn gfx_s3_tint565(out: *mut u16, n8: i32, color: i32, a5: i32, consts: *const u16);
    fn gfx_s3_tint888(out: *mut u8, n16: i32, table: *const u16, w: i32);
    fn gfx_s3_swap565(pixels: *mut u8, n8: i32, mask: *const u32);
}

const PIE: bool = matches!(UNIT, Unit::S3);

#[repr(C, align(16))]
struct Lanes<T, const N: usize>([T; N]);

const fn lanes(values: [u16; 12]) -> [u16; 96] {
    let mut out = [0; 96];
    let mut i = 0;
    while i < 96 {
        out[i] = values[i / 8];
        i += 1;
    }
    out
}

/// In the order MIX565 reads them.
static BLEND_CONSTS: Lanes<u16, 96> = Lanes(lanes([1, 16, 1, 2048, 1, 0x3f, 16, 1, 32, 0x1f, 16, 1]));
static BYTE_MASK: Lanes<u32, 4> = Lanes([0x00ff_00ff; 4]);

/// Pixels of `bytes` each before `ptr` reaches a 16-byte boundary.
fn head(ptr: *const u8, bytes: usize) -> usize {
    let gap = ptr.align_offset(16);
    match bytes {
        2 => gap / 2,
        // 3 * 11 = 1 mod 16.
        _ => gap * 11 % 16,
    }
}

pub(super) unsafe fn blend565(out: *mut u16, back: *const u16, front: *const u16, n: usize, a5: u32) -> usize {
    if !PIE || out.align_offset(2) != 0 {
        return 0;
    }
    let first = head(out.cast(), 2).min(n);
    for i in 0..first {
        unsafe { out.add(i).write(mix565(back.add(i).read(), front.add(i).read(), a5)) };
    }
    let (back, front) = unsafe { (back.add(first), front.add(first)) };
    let mut groups = (n - first) / 8;
    // The loads read a block ahead of unaligned input.
    if groups > 0 && (back.align_offset(16) != 0 || front.align_offset(16) != 0) {
        groups -= 1;
    }
    if groups > 0 {
        unsafe { gfx_s3_blend565(out.add(first), back, front, groups as i32, a5 as i32, BLEND_CONSTS.0.as_ptr()) };
    }
    first + groups * 8
}

pub(super) unsafe fn tint565(out: *mut u16, n: usize, color: u16, a5: u32) -> usize {
    if !PIE || out.align_offset(2) != 0 {
        return 0;
    }
    let first = head(out.cast(), 2).min(n);
    for i in 0..first {
        unsafe { out.add(i).write(mix565(out.add(i).read(), color, a5)) };
    }
    let groups = (n - first) / 8;
    if groups > 0 {
        unsafe { gfx_s3_tint565(out.add(first), groups as i32, i32::from(color), a5 as i32, BLEND_CONSTS.0.as_ptr()) };
    }
    first + groups * 8
}

/// `a` of 256; both weights must fit a byte.
pub(super) unsafe fn tint888(out: *mut u8, n: usize, color: [u8; 3], a: u32) -> usize {
    if !PIE || a == 0 || a >= 256 {
        return 0;
    }
    let first = head(out, 3).min(n);
    for i in 0..first {
        for (ch, &s) in color.iter().enumerate() {
            let p = unsafe { out.add(i * 3 + ch) };
            let v = u32::from(unsafe { p.read() });
            unsafe { p.write(((v * (256 - a) + u32::from(s) * a) >> 8) as u8) };
        }
    }
    let groups = (n - first) / 16;
    if groups > 0 {
        let mut table = Lanes([0u16; 112]);
        table.0[..8].fill(0xff);
        table.0[8..16].fill(1);
        for half in 0..6 {
            for lane in 0..8 {
                let ca = u32::from(color[(half * 8 + lane) % 3]) * a;
                table.0[16 + half * 16 + lane] = (ca & 0xff) as u16;
                table.0[16 + half * 16 + 8 + lane] = (ca >> 8) as u16;
            }
        }
        unsafe { gfx_s3_tint888(out.add(first * 3), groups as i32, table.0.as_ptr(), (256 - a) as i32) };
    }
    first + groups * 16
}

pub(super) unsafe fn swap565(pixels: *mut u8, n: usize) -> usize {
    if !PIE || pixels.align_offset(2) != 0 {
        return 0;
    }
    let first = head(pixels, 2).min(n);
    for i in 0..first {
        unsafe { core::ptr::swap(pixels.add(i * 2), pixels.add(i * 2 + 1)) };
    }
    let groups = (n - first) / 8;
    if groups > 0 {
        unsafe { gfx_s3_swap565(pixels.add(first * 2), groups as i32, BYTE_MASK.0.as_ptr()) };
    }
    first + groups * 8
}
