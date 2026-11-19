/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use super::Hash;

const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
    4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15,
    21,
];

const K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501, 0x698098d8,
    0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340,
    0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87,
    0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
    0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039,
    0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92,
    0xffeff47d, 0x85845dd1, 0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
    0xeb86d391,
];

/// MD5 (RFC 1321). Broken for collisions; only for protocols that name it.
#[derive(Clone)]
pub struct Md5 {
    h: [u32; 4],
    total: u64,
    buf: [u8; 64],
    len: usize,
}

impl Md5 {
    fn compress(&mut self, block: &[u8; 64]) {
        let mut m = [0u32; 16];
        for (word, bytes) in m.iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_le_bytes(bytes.try_into().unwrap());
        }
        let [mut a, mut b, mut c, mut d] = self.h;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        for (h, v) in self.h.iter_mut().zip([a, b, c, d]) {
            *h = h.wrapping_add(v);
        }
    }
}

impl Hash for Md5 {
    const BLOCK: usize = 64;
    type Output = [u8; 16];
    const ZERO: [u8; 16] = [0; 16];

    fn new() -> Self {
        Self { h: [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476], total: 0, buf: [0; 64], len: 0 }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.total += data.len() as u64;
        while !data.is_empty() {
            let n = (64 - self.len).min(data.len());
            self.buf[self.len..self.len + n].copy_from_slice(&data[..n]);
            self.len += n;
            data = &data[n..];
            if self.len == 64 {
                let block = self.buf;
                self.compress(&block);
                self.len = 0;
            }
        }
    }

    fn finalize(mut self) -> [u8; 16] {
        let bits = self.total.wrapping_mul(8);
        let mut pad = [0u8; 64];
        pad[0] = 0x80;
        let n = if self.len < 56 { 56 - self.len } else { 120 - self.len };
        self.update(&pad[..n]);
        self.update(&bits.to_le_bytes());
        let mut out = [0; 16];
        for (chunk, word) in out.chunks_exact_mut(4).zip(self.h) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        out
    }
}
