/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;

use idf::heap::Caps;

use crate::{Error, Result};

/// Bytes in a fixed buffer. Offsets count from the oldest byte held.
pub(crate) struct Ring {
    data: Vec<u8, Caps>,
    head: usize,
    len: usize,
}

impl Ring {
    pub fn new(size: usize, caps: Caps) -> Result<Self> {
        let mut data = Vec::new_in(caps);
        data.try_reserve_exact(size).map_err(|_| Error::NoMemory)?;
        data.resize(size, 0);
        Ok(Self { data, head: 0, len: 0 })
    }

    pub fn capacity(&self) -> usize {
        self.data.len()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn free(&self) -> usize {
        self.data.len() - self.len
    }

    fn at(&self, offset: usize) -> usize {
        let cap = self.data.len();
        let i = self.head + offset;
        if i >= cap { i - cap } else { i }
    }

    /// Copies `src` to `offset` past the held bytes, without holding it yet.
    /// `offset + src.len()` must not exceed `free()`.
    pub fn write_free(&mut self, offset: usize, src: &[u8]) {
        if src.is_empty() {
            return;
        }
        let cap = self.data.len();
        let start = self.at(self.len + offset);
        let first = src.len().min(cap - start);
        self.data[start..start + first].copy_from_slice(&src[..first]);
        self.data[..src.len() - first].copy_from_slice(&src[first..]);
    }

    /// Holds `n` more bytes, written by `write_free` at offset 0.
    pub fn commit(&mut self, n: usize) {
        self.len += n;
    }

    /// As much of `src` as fits.
    pub fn push(&mut self, src: &[u8]) -> usize {
        let n = src.len().min(self.free());
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
