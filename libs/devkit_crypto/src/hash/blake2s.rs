/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use super::Hash;
use crate::{Error, Result};

const IV: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];

const SIGMA: [[u8; 16]; 10] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
];

/// BLAKE2s (RFC 7693), keyed or not, with a digest of 1 to 32 bytes.
#[derive(Clone)]
pub struct Blake2s {
    h: [u32; 8],
    t: u64,
    buf: [u8; 64],
    len: usize,
    out_len: usize,
}

impl Blake2s {
    pub fn with_key(key: &[u8], out_len: usize) -> Result<Self> {
        if key.len() > 32 || !(1..=32).contains(&out_len) {
            return Err(Error::InvalidLength);
        }
        let mut h = IV;
        h[0] ^= 0x0101_0000 ^ ((key.len() as u32) << 8) ^ out_len as u32;
        let mut state = Self { h, t: 0, buf: [0; 64], len: 0, out_len };
        if !key.is_empty() {
            state.buf[..key.len()].copy_from_slice(key);
            state.len = 64;
        }
        Ok(state)
    }

    pub fn out_len(&self) -> usize {
        self.out_len
    }

    /// `out` must be `out_len()` bytes long.
    pub fn finalize_into(mut self, out: &mut [u8]) {
        assert_eq!(out.len(), self.out_len);
        self.t += self.len as u64;
        self.buf[self.len..].fill(0);
        let block = self.buf;
        self.compress(&block, true);
        for (chunk, word) in out.chunks_mut(4).zip(self.h) {
            chunk.copy_from_slice(&word.to_le_bytes()[..chunk.len()]);
        }
    }

    fn compress(&mut self, block: &[u8; 64], last: bool) {
        let mut m = [0u32; 16];
        for (word, bytes) in m.iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_le_bytes(bytes.try_into().unwrap());
        }
        let mut v = [0u32; 16];
        v[..8].copy_from_slice(&self.h);
        v[8..].copy_from_slice(&IV);
        v[12] ^= self.t as u32;
        v[13] ^= (self.t >> 32) as u32;
        if last {
            v[14] = !v[14];
        }
        let g = |v: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize, x: u32, y: u32| {
            v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
            v[d] = (v[d] ^ v[a]).rotate_right(16);
            v[c] = v[c].wrapping_add(v[d]);
            v[b] = (v[b] ^ v[c]).rotate_right(12);
            v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
            v[d] = (v[d] ^ v[a]).rotate_right(8);
            v[c] = v[c].wrapping_add(v[d]);
            v[b] = (v[b] ^ v[c]).rotate_right(7);
        };
        for s in &SIGMA {
            let m = |i: usize| m[s[i] as usize];
            g(&mut v, 0, 4, 8, 12, m(0), m(1));
            g(&mut v, 1, 5, 9, 13, m(2), m(3));
            g(&mut v, 2, 6, 10, 14, m(4), m(5));
            g(&mut v, 3, 7, 11, 15, m(6), m(7));
            g(&mut v, 0, 5, 10, 15, m(8), m(9));
            g(&mut v, 1, 6, 11, 12, m(10), m(11));
            g(&mut v, 2, 7, 8, 13, m(12), m(13));
            g(&mut v, 3, 4, 9, 14, m(14), m(15));
        }
        for i in 0..8 {
            self.h[i] ^= v[i] ^ v[i + 8];
        }
    }
}

impl Hash for Blake2s {
    const BLOCK: usize = 64;
    type Output = [u8; 32];
    const ZERO: [u8; 32] = [0; 32];

    fn new() -> Self {
        Self::with_key(&[], 32).unwrap()
    }

    // The last block is held back until finalize, which must compress it
    // with the final flag even when it is full.
    fn update(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            if self.len == 64 {
                self.t += 64;
                let block = self.buf;
                self.compress(&block, false);
                self.len = 0;
            }
            let n = (64 - self.len).min(data.len());
            self.buf[self.len..self.len + n].copy_from_slice(&data[..n]);
            self.len += n;
            data = &data[n..];
        }
    }

    /// Zero past `out_len()`.
    fn finalize(self) -> [u8; 32] {
        let mut out = [0; 32];
        let len = self.out_len;
        self.finalize_into(&mut out[..len]);
        out
    }
}
