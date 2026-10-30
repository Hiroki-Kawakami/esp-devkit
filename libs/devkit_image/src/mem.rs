/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;

use devkit::Zeroable;
use idf::heap::{self, Caps};

use crate::{HAVE_PIE, Result};

pub type Buffer<T = u8> = Box<[T], Caps>;

/// Where a buffer goes when the caller left `caps` open.
pub fn default_caps() -> Caps {
    Caps::SPIRAM | Caps::ACCESS_8BIT
}

/// Fast memory that SIMD kernels can store to.
pub fn internal_caps() -> Caps {
    if !HAVE_PIE {
        return Caps::INTERNAL | Caps::ACCESS_8BIT;
    }
    // Alone, never OR-ed with INTERNAL: the heap matches caps one priority level
    // at a time and lists the two on different levels for the region most of
    // the free internal memory lives in. Alone it still prefers internal memory
    // and it keeps RTC RAM out, where a kernel's store faults.
    Caps::SIMD
}

pub(crate) fn buffer<T: Zeroable>(len: usize, caps: Option<Caps>) -> Result<Buffer<T>> {
    Ok(heap::zeroed_slice(len, caps.unwrap_or_else(default_caps))?)
}

/// A whole picture about to be overwritten, where clearing it first would cost as
/// much as a good part of the work on it.
///
/// # Safety
/// Every byte must be written before it is read.
pub(crate) unsafe fn uninit_buffer(len: usize, caps: Option<Caps>) -> Result<Buffer> {
    let mut v = heap::try_vec(len, caps.unwrap_or_else(default_caps))?;
    unsafe { v.set_len(len) };
    Ok(v.into_boxed_slice())
}

pub(crate) fn internal_buffer<T: Zeroable>(len: usize) -> Result<Buffer<T>> {
    Ok(heap::zeroed_slice(len, internal_caps())?)
}

/// Small working state, wherever plain `malloc` would put it.
pub(crate) fn small_buffer<T: Zeroable>(len: usize) -> Result<Buffer<T>> {
    Ok(heap::zeroed_slice(len, Caps::DEFAULT)?)
}

pub(crate) fn state<T: Zeroable>() -> Result<Box<T, Caps>> {
    Ok(heap::zeroed_box(Caps::DEFAULT)?)
}
