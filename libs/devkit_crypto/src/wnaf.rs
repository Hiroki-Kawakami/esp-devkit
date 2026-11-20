/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec;
use alloc::vec::Vec;

/// Width-`w` NAF of a little-endian integer: odd digits below 2^(w-1) in
/// magnitude, at most one of any `w` in a row nonzero. Variable time.
pub(crate) fn wnaf(k: &[u32], w: usize) -> Vec<i8> {
    let bits = 32 * k.len();
    let bit = |i: usize| if i < bits { (k[i / 32] >> (i % 32)) & 1 } else { 0 };
    let width = 1u32 << w;
    let mut naf = vec![0i8; bits + w];
    let (mut pos, mut carry) = (0, 0);
    while pos < bits + w {
        let window = carry + (0..w).fold(0, |acc, j| acc | (bit(pos + j) << j));
        if window & 1 == 0 {
            pos += 1;
            continue;
        }
        if window < width / 2 {
            carry = 0;
            naf[pos] = window as i8;
        } else {
            carry = 1;
            naf[pos] = (window as i32 - width as i32) as i8;
        }
        pos += w;
    }
    naf
}
