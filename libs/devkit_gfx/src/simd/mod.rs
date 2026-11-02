/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! Pixel loops on the vector unit of the chip, where it has one. The scalar
//! loops give the same pixels everywhere else; each unit adds a module.

mod p4;

// A build makes only its own chip's.
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub(crate) enum Unit {
    None,
    P4,
}

include!(concat!(env!("OUT_DIR"), "/simd.rs"));

pub(crate) const SPREAD: u32 = 0x07E0_F81F;
// Half of a 5 bit alpha step in each field; without it layers darken by a level
// each, as the shift floors negative differences.
pub(crate) const SPREAD_ROUND: u32 = 0x0200_8010;

#[inline(always)]
pub(crate) const fn spread(v: u16) -> u32 {
    (v as u32 | (v as u32) << 16) & SPREAD
}

/// `front` over `back` by `a5` of 32.
#[inline(always)]
pub fn mix565(back: u16, front: u16, a5: u32) -> u16 {
    let b = spread(back);
    let difference = spread(front).wrapping_sub(b).wrapping_mul(a5).wrapping_add(SPREAD_ROUND);
    let out = b.wrapping_add(difference >> 5) & SPREAD;
    (out | out >> 16) as u16
}

/// `front` over `back` into `out` by `a5` of 32, `n` RGB565 pixels. `out` may
/// be `back`. A task running it keeps its stack out of RTC RAM.
pub unsafe fn blend565(out: *mut u16, back: *const u16, front: *const u16, n: usize, a5: u32) {
    let done = match UNIT {
        Unit::P4 => unsafe { p4::blend565(out, back, front, n, a5) },
        Unit::None => 0,
    };
    for i in done..n {
        unsafe { out.add(i).write(mix565(back.add(i).read(), front.add(i).read(), a5)) };
    }
}

/// `color`, B, G, R, over `n` such pixels at `out` by `alpha`, as
/// `Bgr888::blend` does. A task running it keeps its stack out of RTC RAM.
pub unsafe fn tint888(out: *mut u8, n: usize, color: [u8; 3], alpha: u8) {
    let a = u32::from(alpha) + (u32::from(alpha) >> 7);
    let done = match UNIT {
        Unit::P4 => unsafe { p4::tint888(out, n, color, a) },
        Unit::None => 0,
    };
    for i in done..n {
        for (ch, &s) in color.iter().enumerate() {
            let p = unsafe { out.add(i * 3 + ch) };
            let v = u32::from(unsafe { p.read() });
            unsafe { p.write(((v * (256 - a) + u32::from(s) * a) >> 8) as u8) };
        }
    }
}

/// `color` over `n` RGB565 pixels at `out` by `a5` of 32. A task running it
/// keeps its stack out of RTC RAM.
pub unsafe fn tint565(out: *mut u16, n: usize, color: u16, a5: u32) {
    let done = match UNIT {
        Unit::P4 => unsafe { p4::tint565(out, n, color, a5) },
        Unit::None => 0,
    };
    for i in done..n {
        unsafe { out.add(i).write(mix565(out.add(i).read(), color, a5)) };
    }
}
