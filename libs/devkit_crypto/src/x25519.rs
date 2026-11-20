/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! X25519 (RFC 7748), in constant time.

use crate::fe25519::{self as fe, Fe};

pub(crate) const BASE: [u8; 32] = {
    let mut b = [0; 32];
    b[0] = 9;
    b
};

pub(crate) fn x25519(scalar: &[u8; 32], u: &[u8; 32]) -> [u8; 32] {
    let mut k = *scalar;
    k[0] &= 248;
    k[31] &= 127;
    k[31] |= 64;
    let x1 = fe::from_bytes(u);
    let (mut x2, mut z2, mut x3, mut z3): (Fe, Fe, Fe, Fe) = (fe::ONE, fe::ZERO, x1, fe::ONE);
    let mut swap = 0;
    for t in (0..255).rev() {
        let bit = ((k[t / 8] >> (t % 8)) & 1) as i32;
        swap ^= bit;
        fe::cswap(&mut x2, &mut x3, swap);
        fe::cswap(&mut z2, &mut z3, swap);
        swap = bit;
        let a = fe::add(&x2, &z2);
        let aa = fe::square(&a);
        let b = fe::sub(&x2, &z2);
        let bb = fe::square(&b);
        let e = fe::sub(&aa, &bb);
        let c = fe::add(&x3, &z3);
        let d = fe::sub(&x3, &z3);
        let da = fe::mul(&d, &a);
        let cb = fe::mul(&c, &b);
        x3 = fe::square(&fe::add(&da, &cb));
        z3 = fe::mul(&x1, &fe::square(&fe::sub(&da, &cb)));
        x2 = fe::mul(&aa, &bb);
        z2 = fe::mul(&e, &fe::add(&aa, &fe::mul121665(&e)));
    }
    fe::cswap(&mut x2, &mut x3, swap);
    fe::cswap(&mut z2, &mut z3, swap);
    crate::ct::zeroize(&mut k);
    fe::to_bytes(&fe::mul(&x2, &fe::invert(&z2)))
}
