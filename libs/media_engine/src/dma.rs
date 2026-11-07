/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::alloc::{Allocator, Layout};
use core::ffi::c_void;
use core::mem;
use core::ptr;
use core::sync::atomic::{Ordering, fence};

use idf::cache::{self, Flags};
use idf::heap::Caps;
use idf::{Error, Result};

use crate::sys;

const FIELD: u32 = (1 << 14) - 1;

/// `dma2d_descriptor_t` as its three words of bitfields: bindgen's setters go
/// bit by bit and cost microseconds a descriptor.
#[repr(C, align(8))]
struct Words {
    words: [u32; 3],
    buffer: *mut c_void,
    next: *mut c_void,
}

const _: () = assert!(mem::size_of::<Words>() == mem::size_of::<sys::dma2d_descriptor_t>());

/// A descriptor in internal RAM: the 2D-DMA is given `dma`, the CPU writes it
/// through `cpu`, the uncached alias.
#[derive(Clone, Copy)]
pub(crate) struct Descriptor {
    pub dma: *mut sys::dma2d_descriptor_t,
    cpu: *mut sys::dma2d_descriptor_t,
}

/// What one descriptor says.
pub(crate) struct Fields {
    pub block: (u32, u32),
    pub picture: (u32, u32),
    pub at: (u32, u32),
    pub two_d: bool,
    pub multiple: bool,
    pub eof: bool,
    pub pbyte: u32,
    pub buffer: *const u8,
}

pub(crate) fn internal(size: usize, align: usize) -> Result<*mut u8> {
    let layout = Layout::from_size_align(size, align).map_err(|_| Error::INVALID_ARG)?;
    let block = (Caps::DMA | Caps::INTERNAL).allocate_zeroed(layout).map_err(|_| Error::NO_MEM)?;
    Ok(block.as_ptr().cast())
}

impl Descriptor {
    pub(crate) fn new() -> Result<Self> {
        let align = cache::alignment(Caps::DMA | Caps::INTERNAL);
        let size = mem::size_of::<sys::dma2d_descriptor_t>().next_multiple_of(align);
        let dma: *mut sys::dma2d_descriptor_t = internal(size, align)?.cast();
        // Nothing cached of it may be written back over what the CPU writes
        // through the uncached alias from here on.
        unsafe { cache::msync(dma.cast(), size, Flags::C2M | Flags::INVALIDATE) }?;
        Ok(Self { dma, cpu: cache::uncached(dma) })
    }

    pub(crate) unsafe fn write(&self, f: &Fields) {
        let d = self.cpu.cast::<Words>();
        let owner = sys::DMA2D_DESCRIPTOR_BUFFER_OWNER_DMA;
        unsafe {
            (*d).words = [
                (f.block.1 & FIELD) | (f.block.0 & FIELD) << 14 | u32::from(f.two_d) << 29 | u32::from(f.eof) << 30 | owner << 31,
                (f.picture.1 & FIELD) | (f.picture.0 & FIELD) << 14 | f.pbyte << 28,
                (f.at.1 & FIELD) | (f.at.0 & FIELD) << 14 | u32::from(f.multiple) << 28,
            ];
            (*d).buffer = f.buffer.cast_mut().cast();
            (*d).next = ptr::null_mut();
        }
        fence(Ordering::SeqCst);
    }

    /// Links `next` after it; `None` ends the chain.
    pub(crate) unsafe fn link(&self, next: Option<&Descriptor>) {
        let d = self.cpu.cast::<Words>();
        unsafe { (*d).next = next.map_or(ptr::null_mut(), |n| n.dma.cast()) };
        fence(Ordering::SeqCst);
    }
}

pub(crate) fn pbyte(bits: usize) -> u32 {
    match bits {
        4 => sys::DMA2D_DESCRIPTOR_PBYTE_0B5_PER_PIXEL,
        8 => sys::DMA2D_DESCRIPTOR_PBYTE_1B0_PER_PIXEL,
        12 => sys::DMA2D_DESCRIPTOR_PBYTE_1B5_PER_PIXEL,
        16 => sys::DMA2D_DESCRIPTOR_PBYTE_2B0_PER_PIXEL,
        24 => sys::DMA2D_DESCRIPTOR_PBYTE_3B0_PER_PIXEL,
        _ => sys::DMA2D_DESCRIPTOR_PBYTE_4B0_PER_PIXEL,
    }
}
