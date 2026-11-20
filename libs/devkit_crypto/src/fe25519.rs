/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! Integers modulo 2^255 − 19 in ten limbs of 26 and 25 bits alternately.
//! Every operation takes the same time for any input.

use crate::ct;

pub(crate) type Fe = [i32; 10];

pub(crate) const ZERO: Fe = [0; 10];
pub(crate) const ONE: Fe = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0];

const fn width(i: usize) -> u32 {
    if i % 2 == 0 { 26 } else { 25 }
}

/// Bit 255 is ignored. Limbs are not yet balanced, which `mul` accepts.
pub(crate) const fn from_bytes_raw(s: &[u8; 32]) -> Fe {
    let mut h = [0i32; 10];
    let (mut i, mut pos) = (0, 0);
    while i < 10 {
        let (byte, shift) = (pos / 8, pos % 8);
        let mut v = 0u64;
        let mut k = 0;
        while k < 5 && byte + k < 32 {
            v |= (s[byte + k] as u64) << (8 * k);
            k += 1;
        }
        h[i] = ((v >> shift) & ((1 << width(i)) - 1)) as i32;
        pos += width(i) as usize;
        i += 1;
    }
    h
}

/// Little-endian 16-bit words.
pub(crate) const fn from_words(w: &[u16; 16]) -> Fe {
    let mut b = [0u8; 32];
    let mut i = 0;
    while i < 16 {
        b[2 * i] = w[i] as u8;
        b[2 * i + 1] = (w[i] >> 8) as u8;
        i += 1;
    }
    from_bytes_raw(&b)
}

pub(crate) fn from_bytes(s: &[u8; 32]) -> Fe {
    carry(&mut from_bytes_raw(s).map(i64::from))
}

fn carry(h: &mut [i64; 10]) -> Fe {
    for i in 0..10 {
        let w = width(i);
        let c = (h[i] + (1 << (w - 1))) >> w;
        h[i] -= c << w;
        if i < 9 {
            h[i + 1] += c;
        } else {
            h[0] += 19 * c;
        }
    }
    let c = (h[0] + (1 << 25)) >> 26;
    h[0] -= c << 26;
    h[1] += c;
    h.map(|v| v as i32)
}

/// The canonical encoding.
pub(crate) fn to_bytes(f: &Fe) -> [u8; 32] {
    let mut h = f.map(i64::from);
    let mut q = (19 * h[9] + (1 << 24)) >> 25;
    for i in 0..10 {
        q = (h[i] + q) >> width(i);
    }
    h[0] += 19 * q;
    for i in 0..9 {
        let c = h[i] >> width(i);
        h[i + 1] += c;
        h[i] -= c << width(i);
    }
    h[9] &= (1 << 25) - 1;
    let mut out = [0u8; 32];
    let (mut acc, mut bits, mut n) = (0u64, 0, 0);
    for i in 0..10 {
        acc |= (h[i] as u64) << bits;
        bits += width(i);
        while bits >= 8 {
            out[n] = acc as u8;
            acc >>= 8;
            bits -= 8;
            n += 1;
        }
    }
    out[n] = acc as u8;
    out
}

pub(crate) fn add(f: &Fe, g: &Fe) -> Fe {
    carry(&mut core::array::from_fn(|i| f[i] as i64 + g[i] as i64))
}

pub(crate) fn sub(f: &Fe, g: &Fe) -> Fe {
    carry(&mut core::array::from_fn(|i| f[i] as i64 - g[i] as i64))
}

pub(crate) fn neg(f: &Fe) -> Fe {
    sub(&ZERO, f)
}

pub(crate) fn mul(f: &Fe, g: &Fe) -> Fe {
    let g19: Fe = core::array::from_fn(|j| 19 * g[j]);
    let f2: Fe = core::array::from_fn(|i| if i % 2 == 1 { 2 * f[i] } else { f[i] });
    let mut h = [0i64; 10];
    for i in 0..10 {
        for j in 0..10 {
            let a = if i & j & 1 == 1 { f2[i] } else { f[i] };
            let (b, k) = if i + j >= 10 { (g19[j], i + j - 10) } else { (g[j], i + j) };
            h[k] += a as i64 * b as i64;
        }
    }
    carry(&mut h)
}

pub(crate) fn square(f: &Fe) -> Fe {
    mul(f, f)
}

fn square_n(f: &Fe, n: usize) -> Fe {
    let mut r = *f;
    for _ in 0..n {
        r = square(&r);
    }
    r
}

pub(crate) fn mul121665(f: &Fe) -> Fe {
    carry(&mut f.map(|v| v as i64 * 121665))
}

/// Swaps when `bit` is 1.
pub(crate) fn cswap(f: &mut Fe, g: &mut Fe, bit: i32) {
    let mask = -bit;
    for i in 0..10 {
        let t = mask & (f[i] ^ g[i]);
        f[i] ^= t;
        g[i] ^= t;
    }
}

/// `z^(2^250 − 1)` and `z^11`.
fn pow_250(z: &Fe) -> (Fe, Fe) {
    let z2 = square(z);
    let z9 = mul(&square_n(&z2, 2), z);
    let z11 = mul(&z9, &z2);
    let z_5_0 = mul(&square(&z11), &z9);
    let z_10_0 = mul(&square_n(&z_5_0, 5), &z_5_0);
    let z_20_0 = mul(&square_n(&z_10_0, 10), &z_10_0);
    let z_40_0 = mul(&square_n(&z_20_0, 20), &z_20_0);
    let z_50_0 = mul(&square_n(&z_40_0, 10), &z_10_0);
    let z_100_0 = mul(&square_n(&z_50_0, 50), &z_50_0);
    let z_200_0 = mul(&square_n(&z_100_0, 100), &z_100_0);
    (mul(&square_n(&z_200_0, 50), &z_50_0), z11)
}

/// `1/z`, and 0 for 0.
pub(crate) fn invert(z: &Fe) -> Fe {
    let (z_250_0, z11) = pow_250(z);
    mul(&square_n(&z_250_0, 5), &z11)
}

/// `z^((p − 5) / 8)`.
pub(crate) fn pow22523(z: &Fe) -> Fe {
    mul(&square_n(&pow_250(z).0, 2), z)
}

pub(crate) fn eq(f: &Fe, g: &Fe) -> bool {
    ct::eq(&to_bytes(f), &to_bytes(g))
}

pub(crate) fn is_negative(f: &Fe) -> u8 {
    to_bytes(f)[0] & 1
}
