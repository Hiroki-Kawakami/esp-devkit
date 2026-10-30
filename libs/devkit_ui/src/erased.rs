/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::mem::size_of;
use core::ptr;

/// A message without its type, stored inline in nodes so that handlers cost no
/// allocation when a view is rebuilt.
#[derive(Clone, Copy)]
#[repr(C, align(8))]
pub(crate) struct Erased([u8; 8]);

impl Erased {
    pub(crate) fn new<M: Copy>(message: M) -> Self {
        const { assert!(size_of::<M>() <= 8, "a message is at most 8 bytes") };
        let mut out = Self([0; 8]);
        unsafe { ptr::write(out.0.as_mut_ptr().cast::<M>(), message) };
        out
    }

    /// `M` must be the type it was created from.
    pub(crate) unsafe fn get<M: Copy>(self) -> M {
        unsafe { ptr::read(self.0.as_ptr().cast::<M>()) }
    }
}
