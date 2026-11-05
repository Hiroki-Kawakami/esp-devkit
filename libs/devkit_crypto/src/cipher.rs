/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

fn quarter(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// ChaCha20 (RFC 8439) over `data` in place, from block `counter`.
pub fn chacha20(key: &[u8; 32], nonce: &[u8; 12], counter: u32, data: &mut [u8]) {
    let word = |b: &[u8]| u32::from_le_bytes(b.try_into().unwrap());
    let mut init = [0u32; 16];
    init[..4].copy_from_slice(&[0x61707865, 0x3320646e, 0x79622d32, 0x6b206574]);
    for (i, k) in key.chunks_exact(4).enumerate() {
        init[4 + i] = word(k);
    }
    for (i, n) in nonce.chunks_exact(4).enumerate() {
        init[13 + i] = word(n);
    }
    for (block, chunk) in data.chunks_mut(64).enumerate() {
        init[12] = counter.wrapping_add(block as u32);
        let mut s = init;
        for _ in 0..10 {
            quarter(&mut s, 0, 4, 8, 12);
            quarter(&mut s, 1, 5, 9, 13);
            quarter(&mut s, 2, 6, 10, 14);
            quarter(&mut s, 3, 7, 11, 15);
            quarter(&mut s, 0, 5, 10, 15);
            quarter(&mut s, 1, 6, 11, 12);
            quarter(&mut s, 2, 7, 8, 13);
            quarter(&mut s, 3, 4, 9, 14);
        }
        for (i, byte) in chunk.iter_mut().enumerate() {
            let k = s[i / 4].wrapping_add(init[i / 4]).to_le_bytes()[i % 4];
            *byte ^= k;
        }
    }
}
