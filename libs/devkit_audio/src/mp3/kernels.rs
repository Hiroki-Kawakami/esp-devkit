/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit::A16;

use crate::HAVE_PIE;

use super::tables::{WINDOW_OFF, WINDOW_SPLIT};

/// `v` as `hi * 65536 + lo`, `lo` signed.
#[inline(always)]
pub(super) fn split(hi: &mut i16, lo: &mut i16, v: i32) {
    *hi = (v.wrapping_add(0x8000) >> 16) as i16;
    *lo = v as i16;
}

fn sat32(v: i64) -> i32 {
    crate::sample::sat32(v)
}

/// `out[r] = sat(sum xh * ch) + sat((sum xh * cl + xl * ch) >> 16)` over `n`
/// inputs, about `sum (x * c) >> 32`, for the `[hi x n][lo x n]` rows of `c`.
pub(super) fn dot_scalar(out: &mut [i32], xh: &[i16], xl: &[i16], c: &[i16], rows: usize, n: usize) {
    for (o, row) in out[..rows].iter_mut().zip(c.chunks_exact(2 * n)) {
        let (ch, cl) = row.split_at(n);
        let (mut a, mut b) = (0i64, 0i64);
        for k in 0..n {
            let (h, l) = (i32::from(xh[k]), i32::from(xl[k]));
            a += i64::from(h * i32::from(ch[k]));
            b += i64::from(h * i32::from(cl[k]) + l * i32::from(ch[k]));
        }
        *o = sat32(a).wrapping_add(sat32(b >> 16));
    }
}

/// `acc[j]` for `j < 32` = sum over the window rows `p` of `vh[off_p + j] * c +
/// ((vl[off_p + j] * c) >> 16)`, with `c` = `WINDOW_SPLIT` for output `j`.
pub(super) fn window_scalar(acc: &mut [i32; 32], vh: &[i16], vl: &[i16]) {
    for g in 0..4 {
        for l in 0..8 {
            let j = 8 * g + l;
            let mut sum = 0i32;
            for p in 0..22 {
                let c = i32::from(WINDOW_SPLIT[(22 * g + p) * 8 + l]);
                let off = usize::from(WINDOW_OFF[p]) + j;
                sum = sum.wrapping_add(i32::from(vh[off]) * c).wrapping_add((i32::from(vl[off]) * c) >> 16);
            }
            acc[j] = sum;
        }
    }
}

mod p4 {
    unsafe extern "C" {
        pub safe fn devkit_mp3_k_prepare();
        pub fn devkit_mp3_k_dot16(out: *mut i32, xh: *const i16, xl: *const i16, c: *const i16, rows: i32);
        pub fn devkit_mp3_k_dot24(out: *mut i32, xh: *const i16, xl: *const i16, c: *const i16, rows: i32);
        pub fn devkit_mp3_k_window(acc: *mut i32, vh: *const i16, vl: *const i16, c: *const i16);
    }
}

#[inline(always)]
pub(super) fn prepare() {
    if HAVE_PIE {
        p4::devkit_mp3_k_prepare();
    }
}

/// `n` is 16. `x` and `c` are 16-byte aligned.
#[inline(always)]
pub(super) fn dot16<const N: usize>(out: &mut [i32], xh: &A16<[i16; 16]>, xl: &A16<[i16; 16]>, c: &A16<[i16; N]>, rows: usize) {
    assert!(out.len() >= rows && N >= rows * 32);
    if HAVE_PIE {
        unsafe { p4::devkit_mp3_k_dot16(out.as_mut_ptr(), xh.as_ptr(), xl.as_ptr(), c.as_ptr(), rows as i32) };
    } else {
        dot_scalar(out, &xh[..], &xl[..], &c[..], rows, 16);
    }
}

/// `n` is 24. `x` and `c` are 16-byte aligned.
#[inline(always)]
pub(super) fn dot24(out: &mut [i32], xh: &A16<[i16; 24]>, xl: &A16<[i16; 24]>, c: &A16<[i16; 864]>, rows: usize) {
    assert!(out.len() >= rows && rows * 48 <= c.len());
    if HAVE_PIE {
        unsafe { p4::devkit_mp3_k_dot24(out.as_mut_ptr(), xh.as_ptr(), xl.as_ptr(), c.as_ptr(), rows as i32) };
    } else {
        dot_scalar(out, &xh[..], &xl[..], &c[..], rows, 24);
    }
}

/// `vh` and `vl` are 16-byte aligned.
#[inline(always)]
pub(super) fn window(acc: &mut A16<[i32; 32]>, vh: &[i16], vl: &[i16]) {
    assert!(vh.len() >= 1088 && vl.len() >= 1088);
    debug_assert!(vh.as_ptr().addr() % 16 == 0 && vl.as_ptr().addr() % 16 == 0);
    if HAVE_PIE {
        unsafe { p4::devkit_mp3_k_window(acc.as_mut_ptr(), vh.as_ptr(), vl.as_ptr(), WINDOW_SPLIT.as_ptr()) };
    } else {
        window_scalar(acc, vh, vl);
    }
}
