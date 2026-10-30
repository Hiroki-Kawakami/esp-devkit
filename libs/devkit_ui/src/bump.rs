/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;
use core::alloc::Layout;
use core::cell::{Cell, UnsafeCell};
use core::ptr;

use idf::heap::Caps;

const CHUNK_BYTES: usize = 16 * 1024;
const CHUNK_ALIGN: usize = 16;

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct Block([u8; CHUNK_ALIGN]);

/// Memory for one rebuild. Nothing placed here is dropped.
pub(crate) struct Bump {
    chunks: UnsafeCell<Vec<Vec<Block, Caps>>>,
    offset: Cell<usize>,
    caps: Caps,
}

impl Bump {
    pub(crate) fn new(caps: Caps) -> Self {
        Self { chunks: UnsafeCell::new(Vec::new()), offset: Cell::new(0), caps }
    }

    fn grow(&self, bytes: usize) {
        let blocks = bytes.max(CHUNK_BYTES).div_ceil(CHUNK_ALIGN);
        let mut chunk = Vec::with_capacity_in(blocks, self.caps);
        chunk.resize(blocks, Block([0; CHUNK_ALIGN]));
        unsafe { (*self.chunks.get()).push(chunk) };
        self.offset.set(0);
    }

    fn alloc_raw(&self, layout: Layout) -> *mut u8 {
        assert!(layout.align() <= CHUNK_ALIGN);
        loop {
            let chunks = unsafe { &mut *self.chunks.get() };
            if let Some(chunk) = chunks.last_mut() {
                let start = self.offset.get().next_multiple_of(layout.align());
                if start + layout.size() <= chunk.len() * CHUNK_ALIGN {
                    self.offset.set(start + layout.size());
                    return unsafe { chunk.as_mut_ptr().cast::<u8>().add(start) };
                }
            }
            self.grow(layout.size());
        }
    }

    pub(crate) fn alloc<T>(&self, value: T) -> *mut T {
        let p = self.alloc_raw(Layout::new::<T>()).cast::<T>();
        unsafe { p.write(value) };
        p
    }

    pub(crate) fn alloc_slice<T>(&self, count: usize, read: impl FnOnce(*mut T)) -> *mut T {
        let layout = Layout::array::<T>(count).unwrap();
        let p = self.alloc_raw(layout).cast::<T>();
        read(p);
        p
    }

    pub(crate) fn alloc_str(&self, text: &str) -> *const str {
        let p = self.alloc_slice::<u8>(text.len(), |p| unsafe { ptr::copy_nonoverlapping(text.as_ptr(), p, text.len()) });
        ptr::slice_from_raw_parts(p, text.len()) as *const str
    }

    /// Every reference into it must be gone.
    pub(crate) fn reset(&self) {
        let chunks = unsafe { &mut *self.chunks.get() };
        chunks.truncate(1);
        self.offset.set(0);
    }
}
