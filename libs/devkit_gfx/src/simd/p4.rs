/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! ESP32-P4 PIE, in p4.S. Each call returns how many pixels it did, whole
//! groups of 8; the caller does the rest. Without PIE in the build they do
//! none: the check stays in each, so other builds never link the kernels.

use super::{UNIT, Unit};

unsafe extern "C" {
    fn gfx_p4_blend565(out: *mut u16, back: *const u16, front: *const u16, n8: i32, a5: i32);
    fn gfx_p4_tint565(out: *mut u16, n8: i32, color: i32, a5: i32);
    fn gfx_p4_tint888(out: *mut u8, n16: i32, pattern: *const u8, a: i32);
}

const PIE: bool = matches!(UNIT, Unit::P4);

pub(super) unsafe fn blend565(out: *mut u16, back: *const u16, front: *const u16, n: usize, a5: u32) -> usize {
    let groups = if PIE { n / 8 } else { 0 };
    if groups > 0 {
        unsafe { gfx_p4_blend565(out, back, front, groups as i32, a5 as i32) };
    }
    groups * 8
}

pub(super) unsafe fn tint565(out: *mut u16, n: usize, color: u16, a5: u32) -> usize {
    let groups = if PIE { n / 8 } else { 0 };
    if groups > 0 {
        unsafe { gfx_p4_tint565(out, groups as i32, i32::from(color), a5 as i32) };
    }
    groups * 8
}

/// `a` of 256; both weights must fit a byte.
pub(super) unsafe fn tint888(out: *mut u8, n: usize, color: [u8; 3], a: u32) -> usize {
    let groups = if PIE && a > 0 && a < 256 { n / 16 } else { 0 };
    if groups > 0 {
        let pattern: [u8; 48] = core::array::from_fn(|i| color[i % 3]);
        unsafe { gfx_p4_tint888(out, groups as i32, pattern.as_ptr(), a as i32) };
    }
    groups * 16
}
