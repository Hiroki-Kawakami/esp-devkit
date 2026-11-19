/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

const MOD: u32 = 65521;
// The most bytes before `b` can overflow 32 bits.
const NMAX: usize = 5552;

#[derive(Clone, Copy)]
pub(crate) struct Adler32 {
    a: u32,
    b: u32,
}

impl Adler32 {
    pub(crate) const fn new() -> Self {
        Self { a: 1, b: 0 }
    }

    pub(crate) fn update(&mut self, data: &[u8]) {
        let (mut a, mut b) = (self.a, self.b);
        for chunk in data.chunks(NMAX) {
            for &x in chunk {
                a += u32::from(x);
                b += a;
            }
            a %= MOD;
            b %= MOD;
        }
        (self.a, self.b) = (a, b);
    }

    pub(crate) fn value(&self) -> u32 {
        (self.b << 16) | self.a
    }
}
