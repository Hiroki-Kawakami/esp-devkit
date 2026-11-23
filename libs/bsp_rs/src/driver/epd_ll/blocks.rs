/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! A bit per `BLOCK` pixels of each row of the glass.

use alloc::boxed::Box;
use core::sync::atomic::{AtomicU32, Ordering};

use idf::Result;
use idf::heap::{self, Caps};

pub(crate) const BLOCK: usize = 32;

/// An update is a load and a store, not one atomic step: a row takes one
/// writer at a time.
pub(crate) struct Blocks {
    words: usize,
    bits: Box<[AtomicU32], Caps>,
}

/// Per word, the bits of the blocks holding any of the pixels `x0..x1`.
fn span(x0: usize, x1: usize) -> impl Iterator<Item = (usize, u32)> {
    let (first, last) = (x0 / BLOCK, x1.saturating_sub(1) / BLOCK);
    let words = if x0 < x1 { first / 32..last / 32 + 1 } else { 0..0 };
    words.map(move |w| {
        let (lo, hi) = (first.max(w * 32) - w * 32, last.min(w * 32 + 31) - w * 32);
        (w, (!0u32 >> (31 - (hi - lo))) << lo)
    })
}

impl Blocks {
    /// In internal RAM, all clear.
    pub(crate) fn new(width: usize, height: usize) -> Result<Self> {
        let words = width.div_ceil(BLOCK * 32);
        let mut bits = heap::try_vec(words * height, Caps::INTERNAL)?;
        bits.extend((0..words * height).map(|_| AtomicU32::new(0)));
        Ok(Self { words, bits: bits.into_boxed_slice() })
    }

    /// Row `y`, 32 blocks a word from the left, the leftmost in bit 0.
    pub(crate) fn row(&self, y: usize) -> &[AtomicU32] {
        &self.bits[y * self.words..][..self.words]
    }

    pub(crate) fn has(&self, y: usize, b: usize) -> bool {
        self.row(y)[b / 32].load(Ordering::Relaxed) & 1 << (b % 32) != 0
    }

    pub(crate) fn set(&self, y: usize, b: usize) {
        let word = &self.row(y)[b / 32];
        word.store(word.load(Ordering::Relaxed) | 1 << (b % 32), Ordering::Relaxed);
    }

    /// The blocks holding any of the pixels `x0..x1`.
    pub(crate) fn set_span(&self, y: usize, x0: usize, x1: usize) {
        let row = self.row(y);
        for (w, bits) in span(x0, x1) {
            row[w].store(row[w].load(Ordering::Relaxed) | bits, Ordering::Relaxed);
        }
    }

    pub(crate) fn clear_span(&self, y: usize, x0: usize, x1: usize) {
        let row = self.row(y);
        for (w, bits) in span(x0, x1) {
            let v = row[w].load(Ordering::Relaxed);
            if v & bits != 0 {
                row[w].store(v & !bits, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(blocks: &Blocks, y: usize) -> Vec<u32> {
        blocks.row(y).iter().map(|w| w.load(Ordering::Relaxed)).collect()
    }

    #[test]
    fn spans_cover_the_blocks_touched() {
        assert_eq!(span(0, 1).collect::<Vec<_>>(), [(0, 1)]);
        assert_eq!(span(31, 33).collect::<Vec<_>>(), [(0, 0b11)]);
        assert_eq!(span(0, 960).collect::<Vec<_>>(), [(0, (1 << 30) - 1)]);
        assert_eq!(span(0, 1024).collect::<Vec<_>>(), [(0, !0)]);
        assert_eq!(span(992, 1056).collect::<Vec<_>>(), [(0, 1 << 31), (1, 1)]);
        assert_eq!(span(40, 40).count(), 0);
    }

    #[test]
    fn rows_wider_than_a_word() {
        let blocks = Blocks::new(1872, 2).unwrap();
        assert_eq!(blocks.row(1).len(), 2);
        blocks.set_span(1, 1000, 1088);
        assert_eq!(words(&blocks, 1), [1 << 31, 0b11]);
        assert_eq!(words(&blocks, 0), [0, 0]);
        assert!(blocks.has(1, 31) && blocks.has(1, 33) && !blocks.has(1, 34));
        blocks.clear_span(1, 1024, 1056);
        assert_eq!(words(&blocks, 1), [1 << 31, 0b10]);
        blocks.set(0, 58);
        assert_eq!(words(&blocks, 0), [0, 1 << 26]);
    }
}
