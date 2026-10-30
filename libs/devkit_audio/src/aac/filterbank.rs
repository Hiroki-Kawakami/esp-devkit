/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use super::kernels::{Tables, dct4_128, dct4_1024, mul15};
use super::tables::*;
use super::{Chan, EIGHT_SHORT, FRAME_LEN, LONG_START, LONG_STOP};

#[inline(always)]
unsafe fn zat(z: *const i32, m: usize) -> i32 {
    unsafe { if m & 1 != 0 { *z.add(1023 - (m >> 1)) } else { *z.add(m >> 1) } }
}

#[inline(always)]
fn m15(a: i32, c: i16) -> i32 {
    mul15(a, i32::from(c))
}

unsafe fn eight_short(c: &mut Chan, scratch: *mut i32, time: *mut i32, prev_win: &[i16; 128], win: &[i16; 128]) {
    unsafe {
        let buf = scratch;
        let tmp = scratch.add(2 * FRAME_LEN);
        core::ptr::write_bytes(buf, 0, 2 * FRAME_LEN);
        for w in 0..8 {
            let z = c.spec.as_mut_ptr().add(128 * w);
            dct4_128(z, tmp);
            let rise = if w != 0 { win } else { prev_win };
            let dst = buf.add(448 + 128 * w);
            for n in 0..64 {
                *dst.add(n) = (*dst.add(n)).wrapping_add(m15(*z.add(64 + n), rise[n]));
            }
            for n in 0..64 {
                *dst.add(64 + n) = (*dst.add(64 + n)).wrapping_sub(m15(*z.add(127 - n), rise[64 + n]));
            }
            for n in 0..64 {
                *dst.add(128 + n) = (*dst.add(128 + n)).wrapping_sub(m15(*z.add(63 - n), win[127 - n]));
            }
            for n in 0..64 {
                *dst.add(192 + n) = (*dst.add(192 + n)).wrapping_sub(m15(*z.add(n), win[63 - n]));
            }
        }
        // `time` may be `buf + FRAME_LEN`.
        for n in 0..FRAME_LEN {
            let t = c.overlap[n].wrapping_add(*buf.add(n));
            c.overlap[n] = *buf.add(FRAME_LEN + n);
            *time.add(n) = t;
        }
    }
}

/// `scratch` holds 3 * FRAME_LEN words and is 16-byte aligned; `time` receives
/// FRAME_LEN words and may be `scratch + FRAME_LEN`.
pub(super) unsafe fn filterbank(c: &mut Chan, tables: &Tables, scratch: *mut i32, time: *mut i32) {
    let shape = c.ics.window_shape;
    let sequence = c.ics.window_sequence;
    let long_win = unsafe { &*tables.win_long[usize::from(shape)] };
    let short_win = if shape { &AAC_WIN_KBD_SHORT } else { &AAC_WIN_SINE_SHORT };
    let prev_long = unsafe { &*tables.win_long[usize::from(c.prev_shape)] };
    let prev_short = if c.prev_shape { &AAC_WIN_KBD_SHORT } else { &AAC_WIN_SINE_SHORT };
    c.prev_shape = shape;

    if sequence == EIGHT_SHORT {
        unsafe { eight_short(c, scratch, time, prev_short, short_win) };
        return;
    }

    let z = c.spec.as_mut_ptr();
    let overlap = c.overlap.as_mut_ptr();
    unsafe {
        dct4_1024(z, scratch, tables.dct1024.as_ptr());
        let z = z.cast_const();
        if sequence != LONG_STOP {
            for h in 0..256 {
                let n = 2 * h;
                *time.add(n) = (*overlap.add(n)).wrapping_add(m15(*z.add(256 + n / 2), prev_long[n]));
                *time.add(n + 1) = (*overlap.add(n + 1)).wrapping_add(m15(*z.add(767 - n / 2), prev_long[n + 1]));
            }
            for h in 0..256 {
                let n = 2 * h;
                *time.add(512 + n) = (*overlap.add(512 + n)).wrapping_sub(m15(*z.add(512 + n / 2), prev_long[512 + n]));
                *time.add(513 + n) = (*overlap.add(513 + n)).wrapping_sub(m15(*z.add(511 - n / 2), prev_long[513 + n]));
            }
        } else {
            for n in 0..448 {
                *time.add(n) = *overlap.add(n);
            }
            for n in 448..512 {
                *time.add(n) = (*overlap.add(n)).wrapping_add(m15(zat(z, 512 + n), prev_short[n - 448]));
            }
            for n in 512..576 {
                *time.add(n) = (*overlap.add(n)).wrapping_sub(m15(zat(z, 1535 - n), prev_short[n - 448]));
            }
            for n in 576..1024 {
                *time.add(n) = (*overlap.add(n)).wrapping_sub(zat(z, 1535 - n));
            }
        }
        if sequence != LONG_START {
            for h in 0..256 {
                let n = 2 * h;
                *overlap.add(n) = m15(*z.add(768 + n / 2), long_win[1023 - n]).wrapping_neg();
                *overlap.add(n + 1) = m15(*z.add(255 - n / 2), long_win[1022 - n]).wrapping_neg();
            }
            for h in 0..256 {
                let n = 2 * h;
                *overlap.add(512 + n) = m15(*z.add(n / 2), long_win[511 - n]).wrapping_neg();
                *overlap.add(513 + n) = m15(*z.add(1023 - n / 2), long_win[510 - n]).wrapping_neg();
            }
        } else {
            for n in 0..448 {
                *overlap.add(n) = zat(z, 511 - n).wrapping_neg();
            }
            for n in 448..512 {
                *overlap.add(n) = m15(zat(z, 511 - n), short_win[575 - n]).wrapping_neg();
            }
            for n in 512..576 {
                *overlap.add(n) = m15(zat(z, n - 512), short_win[575 - n]).wrapping_neg();
            }
            for n in 576..1024 {
                *overlap.add(n) = 0;
            }
        }
    }
}
