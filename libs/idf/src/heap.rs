/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::alloc::{AllocError, Allocator, Layout};
use core::ptr::{self, NonNull};

use crate::sys;

// HEAP_HAS_EXEC_HEAP has no prompt and is absent from sdkconfig, and so unknown
// to --check-cfg, when it is off.
#[allow(unexpected_cfgs)]
mod caps {
    use crate::sys;

    devkit::bitflags! {
        pub struct Caps: u32 {
            #[cfg(CONFIG_HEAP_HAS_EXEC_HEAP)]
            const EXEC = sys::MALLOC_CAP_EXEC;
            const ACCESS_32BIT = sys::MALLOC_CAP_32BIT;
            const ACCESS_8BIT = sys::MALLOC_CAP_8BIT;
            const DMA = sys::MALLOC_CAP_DMA;
            const SPIRAM = sys::MALLOC_CAP_SPIRAM;
            const INTERNAL = sys::MALLOC_CAP_INTERNAL;
            const DEFAULT = sys::MALLOC_CAP_DEFAULT;
            const IRAM_8BIT = sys::MALLOC_CAP_IRAM_8BIT;
            const RETENTION = sys::MALLOC_CAP_RETENTION;
            const RTCRAM = sys::MALLOC_CAP_RTCRAM;
            const SPM = sys::MALLOC_CAP_SPM;
            const DMA_DESC_AHB = sys::MALLOC_CAP_DMA_DESC_AHB;
            const DMA_DESC_AXI = sys::MALLOC_CAP_DMA_DESC_AXI;
            const CACHE_ALIGNED = sys::MALLOC_CAP_CACHE_ALIGNED;
            const SIMD = sys::MALLOC_CAP_SIMD;
            const SPIRAM_NO_ENC = sys::MALLOC_CAP_SPIRAM_NO_ENC;
        }
    }
}

pub use caps::Caps;

// TLSF only guarantees 4-byte alignment; anything stricter goes through the
// aligned calls.
#[cfg(target_os = "none")]
const MALLOC_ALIGN: usize = 4;
#[cfg(not(target_os = "none"))]
const MALLOC_ALIGN: usize = 16;

fn dangling(layout: Layout) -> NonNull<[u8]> {
    let p = unsafe { NonNull::new_unchecked(ptr::without_provenance_mut(layout.align())) };
    NonNull::slice_from_raw_parts(p, 0)
}

fn block(p: *mut core::ffi::c_void, size: usize) -> Result<NonNull<[u8]>, AllocError> {
    let p = NonNull::new(p.cast::<u8>()).ok_or(AllocError)?;
    Ok(NonNull::slice_from_raw_parts(p, size))
}

impl Caps {
    fn alloc(&self, layout: Layout, zeroed: bool) -> Result<NonNull<[u8]>, AllocError> {
        let (size, align, caps) = (layout.size(), layout.align(), self.bits());
        if size == 0 {
            return Ok(dangling(layout));
        }
        let p = unsafe {
            match (align <= MALLOC_ALIGN, zeroed) {
                (true, false) => sys::heap_caps_malloc(size, caps),
                (true, true) => sys::heap_caps_calloc(1, size, caps),
                (false, false) => sys::heap_caps_aligned_alloc(align, size, caps),
                (false, true) => sys::heap_caps_aligned_calloc(align, 1, size, caps),
            }
        };
        block(p, size)
    }

    unsafe fn resize(&self, ptr: NonNull<u8>, old: Layout, new: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if old.size() != 0 && new.size() != 0 && old.align().max(new.align()) <= MALLOC_ALIGN {
            let p = unsafe { sys::heap_caps_realloc(ptr.as_ptr().cast(), new.size(), self.bits()) };
            return block(p, new.size());
        }
        let moved = self.alloc(new, false)?;
        unsafe {
            ptr::copy_nonoverlapping(ptr.as_ptr(), moved.cast::<u8>().as_ptr(), old.size().min(new.size()));
            self.deallocate(ptr, old);
        }
        Ok(moved)
    }
}

unsafe impl Allocator for Caps {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.alloc(layout, false)
    }

    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.alloc(layout, true)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        if layout.size() != 0 {
            unsafe { sys::heap_caps_free(ptr.as_ptr().cast()) }
        }
    }

    unsafe fn grow(&self, ptr: NonNull<u8>, old: Layout, new: Layout) -> Result<NonNull<[u8]>, AllocError> {
        unsafe { self.resize(ptr, old, new) }
    }

    unsafe fn shrink(&self, ptr: NonNull<u8>, old: Layout, new: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if new.size() == 0 {
            unsafe { self.deallocate(ptr, old) };
            return Ok(dangling(new));
        }
        unsafe { self.resize(ptr, old, new) }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Info {
    pub total_free_bytes: usize,
    pub total_allocated_bytes: usize,
    pub largest_free_block: usize,
    pub minimum_free_bytes: usize,
    pub allocated_blocks: usize,
    pub free_blocks: usize,
    pub total_blocks: usize,
}

pub fn free_size(caps: Caps) -> usize {
    unsafe { sys::heap_caps_get_free_size(caps.bits()) }
}

pub fn minimum_free_size(caps: Caps) -> usize {
    unsafe { sys::heap_caps_get_minimum_free_size(caps.bits()) }
}

pub fn largest_free_block(caps: Caps) -> usize {
    unsafe { sys::heap_caps_get_largest_free_block(caps.bits()) }
}

pub fn total_size(caps: Caps) -> usize {
    unsafe { sys::heap_caps_get_total_size(caps.bits()) }
}

pub fn info(caps: Caps) -> Info {
    let mut i: sys::multi_heap_info_t = unsafe { core::mem::zeroed() };
    unsafe { sys::heap_caps_get_info(&mut i, caps.bits()) };
    Info {
        total_free_bytes: i.total_free_bytes,
        total_allocated_bytes: i.total_allocated_bytes,
        largest_free_block: i.largest_free_block,
        minimum_free_bytes: i.minimum_free_bytes,
        allocated_blocks: i.allocated_blocks,
        free_blocks: i.free_blocks,
        total_blocks: i.total_blocks,
    }
}

#[cfg(test)]
mod tests {
    use super::{Caps, free_size, info, largest_free_block};
    use core::alloc::{Allocator, Layout};

    #[repr(C, align(64))]
    #[derive(Clone, Copy, PartialEq, Debug)]
    struct Line([u8; 64]);

    #[test]
    fn vec_grows_and_shrinks() {
        let mut v: Vec<u32, Caps> = Vec::with_capacity_in(4, Caps::DMA | Caps::INTERNAL);
        v.extend(0..1000);
        assert_eq!(v.iter().sum::<u32>(), 499_500);
        v.truncate(10);
        v.shrink_to_fit();
        assert_eq!(v, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn honors_large_alignment() {
        let b = Box::new_in(Line([7; 64]), Caps::SPIRAM);
        assert_eq!(&*b as *const Line as usize % 64, 0);
        let mut v: Vec<Line, Caps> = Vec::new_in(Caps::DMA);
        for i in 0..20 {
            v.push(Line([i; 64]));
            assert_eq!(v.as_ptr() as usize % 64, 0);
        }
        v.truncate(3);
        v.shrink_to_fit();
        assert_eq!(v[2], Line([2; 64]));
    }

    #[test]
    fn zeroed_and_zero_sized() {
        for align in [1, 64] {
            let layout = Layout::from_size_align(256, align).unwrap();
            let p = Caps::INTERNAL.allocate_zeroed(layout).unwrap();
            assert!(unsafe { p.as_ref() }.iter().all(|&b| b == 0));
            unsafe { Caps::INTERNAL.deallocate(p.cast(), layout) };
        }
        let layout = Layout::from_size_align(0, 32).unwrap();
        let p = Caps::DMA.allocate(layout).unwrap();
        assert_eq!(p.len(), 0);
        assert_eq!(p.cast::<u8>().as_ptr() as usize, 32);
        unsafe { Caps::DMA.deallocate(p.cast(), layout) };
    }

    #[test]
    fn bits_match_idf() {
        assert_eq!(Caps::ACCESS_8BIT.bits(), 1 << 2);
        assert_eq!(Caps::DMA.bits(), 1 << 3);
        assert_eq!(Caps::SPIRAM.bits(), 1 << 10);
        assert_eq!(Caps::INTERNAL.bits(), 1 << 11);
        assert_eq!(Caps::SPIRAM_NO_ENC.bits(), 1 << 21);
    }

    #[test]
    fn queries() {
        assert!(free_size(Caps::DEFAULT) > 0);
        assert!(largest_free_block(Caps::DMA) > 0);
        assert_eq!(info(Caps::DEFAULT).total_free_bytes, free_size(Caps::DEFAULT));
    }
}
