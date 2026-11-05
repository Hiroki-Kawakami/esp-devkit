/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::ct;
use crate::hash::Hash;

/// HMAC (RFC 2104) over any [`Hash`].
#[derive(Clone)]
pub struct Hmac<H: Hash> {
    inner: H,
    outer: H,
}

impl<H: Hash> Hmac<H> {
    pub fn new(key: &[u8]) -> Self {
        let mut block = [0u8; 128];
        let block = &mut block[..H::BLOCK];
        if key.len() > H::BLOCK {
            let digest = H::digest(key);
            block[..digest.as_ref().len()].copy_from_slice(digest.as_ref());
        } else {
            block[..key.len()].copy_from_slice(key);
        }
        let mut inner = H::new();
        let mut outer = H::new();
        for byte in block.iter_mut() {
            *byte ^= 0x36;
        }
        inner.update(block);
        for byte in block.iter_mut() {
            *byte ^= 0x36 ^ 0x5c;
        }
        outer.update(block);
        ct::zeroize(block);
        Self { inner, outer }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    pub fn finalize(self) -> H::Output {
        let mut outer = self.outer;
        outer.update(self.inner.finalize().as_ref());
        outer.finalize()
    }

    /// Compares in constant time; `tag` may be a truncation of the full output.
    pub fn verify(self, tag: &[u8]) -> bool {
        let full = self.finalize();
        !tag.is_empty() && tag.len() <= full.as_ref().len() && ct::eq(&full.as_ref()[..tag.len()], tag)
    }

    pub fn mac(key: &[u8], data: &[u8]) -> H::Output {
        let mut mac = Self::new(key);
        mac.update(data);
        mac.finalize()
    }
}

/// SipHash-2-4.
pub fn siphash(k0: u64, k1: u64, data: &[u8]) -> u64 {
    let mut v = [k0 ^ 0x736f6d6570736575, k1 ^ 0x646f72616e646f6d, k0 ^ 0x6c7967656e657261, k1 ^ 0x7465646279746573];
    let round = |v: &mut [u64; 4]| {
        v[0] = v[0].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(13) ^ v[0];
        v[0] = v[0].rotate_left(32);
        v[2] = v[2].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(16) ^ v[2];
        v[0] = v[0].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(21) ^ v[0];
        v[2] = v[2].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(17) ^ v[2];
        v[2] = v[2].rotate_left(32);
    };
    let mut chunks = data.chunks_exact(8);
    for chunk in &mut chunks {
        let m = u64::from_le_bytes(chunk.try_into().unwrap());
        v[3] ^= m;
        round(&mut v);
        round(&mut v);
        v[0] ^= m;
    }
    let mut last = [0u8; 8];
    last[..chunks.remainder().len()].copy_from_slice(chunks.remainder());
    last[7] = data.len() as u8;
    let m = u64::from_le_bytes(last);
    v[3] ^= m;
    round(&mut v);
    round(&mut v);
    v[0] ^= m;
    v[2] ^= 0xff;
    for _ in 0..4 {
        round(&mut v);
    }
    v[0] ^ v[1] ^ v[2] ^ v[3]
}
