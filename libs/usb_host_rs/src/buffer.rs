/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::alloc::{Allocator, Layout};
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;

use idf::heap::Caps;

use crate::{Error, Result};

/// Transfer memory, starting and ending on a cache line so the controller
/// may write it directly.
pub struct Buffer {
    ptr: NonNull<u8>,
    len: usize,
    caps: Caps,
}

unsafe impl Send for Buffer {}
unsafe impl Sync for Buffer {}

impl Buffer {
    /// A cache line: where the controller may start writing.
    #[cfg(CONFIG_CACHE_L2_CACHE_LINE_128B)]
    pub const ALIGN: usize = 128;
    #[cfg(not(CONFIG_CACHE_L2_CACHE_LINE_128B))]
    pub const ALIGN: usize = 64;

    /// Zeroed.
    pub fn new(len: usize, caps: Caps) -> Result<Self> {
        let ptr = caps.allocate_zeroed(Self::layout(len)).map_err(|_| Error::NoMemory)?;
        Ok(Self { ptr: ptr.cast(), len, caps })
    }

    pub(crate) fn dma(&self) -> (*mut u8, usize) {
        (self.ptr.as_ptr(), Self::layout(self.len).size())
    }

    fn layout(len: usize) -> Layout {
        Layout::from_size_align(len.next_multiple_of(Self::ALIGN), Self::ALIGN).unwrap()
    }
}

impl Deref for Buffer {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl DerefMut for Buffer {
    fn deref_mut(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe { self.caps.deallocate(self.ptr, Self::layout(self.len)) };
    }
}
