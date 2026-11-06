/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::heap::Caps;
use crate::Result;
#[cfg(target_os = "none")]
use crate::{Error, sys};

devkit::bitflags! {
    /// `ESP_CACHE_MSYNC_FLAG_*`.
    pub struct Flags: u32 {
        const INVALIDATE = 1 << 0;
        /// Lets `ptr` and `len` fall inside cache lines.
        const UNALIGNED = 1 << 1;
        /// Writes the cache back to memory, before a DMA reads it.
        const C2M = 1 << 2;
        /// Drops the cache over memory a DMA wrote.
        const M2C = 1 << 3;
    }
}

/// `esp_cache_msync`; nothing for `len` 0, and nothing in the simulator.
///
/// # Safety
/// `ptr..ptr + len` is memory the caller owns. `M2C` discards what the CPU
/// wrote there and has not written back.
pub unsafe fn msync(ptr: *const u8, len: usize, flags: Flags) -> Result<()> {
    if len == 0 {
        return Ok(());
    }
    #[cfg(target_os = "none")]
    {
        Error::check(unsafe { sys::esp_cache_msync(ptr.cast_mut().cast(), len, flags.bits() as i32) })
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (ptr, flags);
        Ok(())
    }
}

/// The cache line of memory with `caps`: what a DMA buffer there starts and
/// ends on. 4 where it is not cached.
pub fn alignment(caps: Caps) -> usize {
    #[cfg(target_os = "none")]
    {
        let mut out = 0;
        unsafe { sys::esp_cache_get_alignment(caps.bits(), &mut out) };
        out.max(4)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = caps;
        4
    }
}

/// The non-cacheable alias of internal RAM at `ptr`, through which the CPU
/// never shares a cache line with a DMA; `ptr` itself where internal RAM is
/// not cached.
pub fn uncached<T>(ptr: *mut T) -> *mut T {
    #[cfg(target_os = "none")]
    {
        unsafe { sys::idf_cache_uncached(ptr.cast()) }.cast()
    }
    #[cfg(not(target_os = "none"))]
    {
        ptr
    }
}
