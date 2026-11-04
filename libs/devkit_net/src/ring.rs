/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;

use idf::heap::Caps;

/// Bytes in a buffer that grows up to its capacity as they arrive. Offsets
/// count from the oldest byte held.
pub(crate) struct Ring {
    data: Vec<u8, Caps>,
    capacity: usize,
    head: usize,
    len: usize,
}

impl Ring {
    pub fn new(capacity: usize, caps: Caps) -> Self {
        Self { data: Vec::new_in(caps), capacity, head: 0, len: 0 }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn free(&self) -> usize {
        self.capacity - self.len
    }

    fn at(&self, offset: usize) -> usize {
        let cap = self.data.len();
        let i = self.head + offset;
        if i >= cap { i - cap } else { i }
    }

    /// Room for `end` bytes past the oldest; doubling, then exactly `end`
    /// when doubling does not fit in memory.
    fn reserve(&mut self, end: usize) -> bool {
        let old = self.data.len();
        if end <= old {
            return true;
        }
        let doubled = (old * 2).max(end).min(self.capacity);
        let Some(new) = [doubled, end].into_iter().find(|&n| self.data.try_reserve_exact(n - old).is_ok()) else {
            return false;
        };
        self.data.resize(new, 0);
        self.data[..old].rotate_left(self.head);
        self.head = 0;
        true
    }

    /// Copies `src` to `offset` past the held bytes, without holding it yet.
    /// `offset + src.len()` must not exceed `free()`. `false` when there was
    /// no memory for it.
    pub fn write_free(&mut self, offset: usize, src: &[u8]) -> bool {
        if src.is_empty() {
            return true;
        }
        if !self.reserve(self.len + offset + src.len()) {
            return false;
        }
        let cap = self.data.len();
        let start = self.at(self.len + offset);
        let first = src.len().min(cap - start);
        self.data[start..start + first].copy_from_slice(&src[..first]);
        self.data[..src.len() - first].copy_from_slice(&src[first..]);
        true
    }

    /// Holds `n` more bytes, written by `write_free` at offset 0.
    pub fn commit(&mut self, n: usize) {
        self.len += n;
    }

    /// As much of `src` as fits, in capacity and then in memory.
    pub fn push(&mut self, src: &[u8]) -> usize {
        let mut n = src.len().min(self.free());
        if !self.reserve(self.len + n) {
            n = self.data.len() - self.len;
        }
        self.write_free(0, &src[..n]);
        self.commit(n);
        n
    }

    /// Copies held bytes from `offset` into `out`, as many as there are.
    pub fn read_at(&self, offset: usize, out: &mut [u8]) -> usize {
        let n = out.len().min(self.len.saturating_sub(offset));
        if n == 0 {
            return 0;
        }
        let cap = self.data.len();
        let start = self.at(offset);
        let first = n.min(cap - start);
        out[..first].copy_from_slice(&self.data[start..start + first]);
        out[first..n].copy_from_slice(&self.data[..n - first]);
        n
    }

    pub fn discard(&mut self, n: usize) {
        if n > 0 {
            self.head = self.at(n);
            self.len -= n;
        }
    }

    /// Takes `n` bytes and copies as many of them as fit into `out`.
    pub fn pop(&mut self, n: usize, out: &mut [u8]) -> usize {
        let copy = n.min(out.len());
        let copied = self.read_at(0, &mut out[..copy]);
        self.discard(n);
        copied
    }
}
