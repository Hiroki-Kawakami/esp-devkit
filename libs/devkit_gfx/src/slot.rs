/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU8, AtomicUsize, Ordering};

const EMPTY: u8 = 0;
const BINDING: u8 = 1;
const BOUND: u8 = 2;

/// Resource bytes that become available at run time, set once.
pub struct ResourceSlot {
    state: AtomicU8,
    ptr: AtomicPtr<u8>,
    len: AtomicUsize,
}

impl ResourceSlot {
    pub const fn new() -> Self {
        Self { state: AtomicU8::new(EMPTY), ptr: AtomicPtr::new(ptr::null_mut()), len: AtomicUsize::new(0) }
    }

    /// False, leaving the slot as it is, when it was bound already.
    pub fn bind(&self, data: &'static [u8]) -> bool {
        if self.state.compare_exchange(EMPTY, BINDING, Ordering::Acquire, Ordering::Relaxed).is_err() {
            return false;
        }
        self.ptr.store(data.as_ptr().cast_mut(), Ordering::Relaxed);
        self.len.store(data.len(), Ordering::Relaxed);
        self.state.store(BOUND, Ordering::Release);
        true
    }

    pub fn get(&self) -> Option<&'static [u8]> {
        if self.state.load(Ordering::Acquire) != BOUND {
            return None;
        }
        Some(unsafe { core::slice::from_raw_parts(self.ptr.load(Ordering::Relaxed), self.len.load(Ordering::Relaxed)) })
    }
}

impl Default for ResourceSlot {
    fn default() -> Self {
        Self::new()
    }
}
