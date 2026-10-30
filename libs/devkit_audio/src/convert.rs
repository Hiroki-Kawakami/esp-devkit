/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::Sample;

/// Interchange layouts at the edges of a chain, little-endian. `S24` is packed
/// 3-byte; `S32` is full-range, which also covers 24-bit MSB-justified in 32.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PcmLayout {
    S16,
    S24,
    S32,
}

const S16_SHIFT: u32 = 8;
const S24_MAX: i32 = (1 << 23) - 1;
const S24_MIN: i32 = -(1 << 23);

impl PcmLayout {
    pub const fn bytes(self) -> usize {
        match self {
            PcmLayout::S16 => 2,
            PcmLayout::S24 => 3,
            PcmLayout::S32 => 4,
        }
    }
}

fn load(layout: PcmLayout, p: &[u8]) -> i32 {
    match layout {
        PcmLayout::S24 => i32::from_le_bytes([0, p[0], p[1], p[2]]) >> 8,
        PcmLayout::S32 => i32::from_le_bytes([p[0], p[1], p[2], p[3]]) >> 8,
        PcmLayout::S16 => i32::from(i16::from_le_bytes([p[0], p[1]])) * (1 << S16_SHIFT),
    }
}

fn store(layout: PcmLayout, p: &mut [u8], v: i32) {
    match layout {
        PcmLayout::S16 => p.copy_from_slice(&i16::store(i64::from(v)).to_le_bytes()),
        PcmLayout::S24 => p.copy_from_slice(&v.clamp(S24_MIN, S24_MAX).to_le_bytes()[..3]),
        PcmLayout::S32 => p.copy_from_slice(&((v.clamp(S24_MIN, S24_MAX) as u32) << 8).to_le_bytes()),
    }
}

/// Converts `dst.len()` samples. Narrowing saturates.
pub fn from_pcm<S: Sample>(src: &[u8], layout: PcmLayout, dst: &mut [S]) {
    let bytes = layout.bytes();
    assert!(src.len() >= dst.len() * bytes);
    for (d, s) in dst.iter_mut().zip(src.chunks_exact(bytes)) {
        *d = S::store(i64::from(load(layout, s)));
    }
}

/// Converts `src.len()` samples. Narrowing saturates.
pub fn to_pcm<S: Sample>(src: &[S], dst: &mut [u8], layout: PcmLayout) {
    let bytes = layout.bytes();
    assert!(dst.len() >= src.len() * bytes);
    for (s, d) in src.iter().zip(dst.chunks_exact_mut(bytes)) {
        store(layout, d, s.load());
    }
}

/// Converts `src.len()` samples between module formats. Narrowing saturates.
pub fn convert<A: Sample, B: Sample>(src: &[A], dst: &mut [B]) {
    assert!(dst.len() >= src.len());
    for (d, s) in dst.iter_mut().zip(src) {
        *d = B::store(i64::from(s.load()));
    }
}
