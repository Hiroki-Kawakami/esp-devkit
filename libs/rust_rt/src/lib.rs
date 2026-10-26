/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![no_std]
#![feature(ptr_alignment_type)]
#![feature(rustc_attrs)]
#![allow(internal_features)]

use core::ffi::{c_char, c_void};
use core::fmt::{self, Write};
use core::mem::Alignment;
use core::panic::PanicInfo;
use core::ptr::{self, NonNull};

unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn calloc(nmemb: usize, size: usize) -> *mut c_void;
    fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
    fn memalign(alignment: usize, size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
    fn esp_system_abort(details: *const c_char) -> !;
}

// TLSF only guarantees 4-byte alignment; anything stricter goes through memalign.
const MALLOC_ALIGN: usize = 4;

// rustc only generates the allocator shim when it links the final binary, which
// it never does here. These signatures must match the extern block in the
// pinned toolchain's library/alloc/src/alloc.rs.
#[rustc_std_internal_symbol]
unsafe fn __rust_alloc(size: usize, align: Alignment) -> *mut u8 {
    unsafe {
        if align.as_usize() <= MALLOC_ALIGN {
            malloc(size).cast()
        } else {
            memalign(align.as_usize(), size).cast()
        }
    }
}

#[rustc_std_internal_symbol]
unsafe fn __rust_alloc_zeroed(size: usize, align: Alignment) -> *mut u8 {
    unsafe {
        if align.as_usize() <= MALLOC_ALIGN {
            return calloc(1, size).cast();
        }
        let p: *mut u8 = memalign(align.as_usize(), size).cast();
        if !p.is_null() {
            ptr::write_bytes(p, 0, size);
        }
        p
    }
}

#[rustc_std_internal_symbol]
unsafe fn __rust_dealloc(ptr: NonNull<u8>, _size: usize, _align: Alignment) {
    unsafe { free(ptr.as_ptr().cast()) }
}

#[rustc_std_internal_symbol]
unsafe fn __rust_realloc(ptr: NonNull<u8>, old_size: usize, align: Alignment, new_size: usize) -> *mut u8 {
    unsafe {
        if align.as_usize() <= MALLOC_ALIGN {
            return realloc(ptr.as_ptr().cast(), new_size).cast();
        }
        let new: *mut u8 = memalign(align.as_usize(), new_size).cast();
        if !new.is_null() {
            ptr::copy_nonoverlapping(ptr.as_ptr(), new, old_size.min(new_size));
            free(ptr.as_ptr().cast());
        }
        new
    }
}

#[rustc_std_internal_symbol]
fn __rust_alloc_error_handler(size: usize, align: usize) -> ! {
    panic!("allocation of {size} bytes (align {align}) failed")
}

#[rustc_std_internal_symbol]
fn __rust_no_alloc_shim_is_unstable_v2() {}

struct AbortMessage {
    buf: [u8; 192],
    len: usize,
}

impl Write for AbortMessage {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let room = self.buf.len() - 1 - self.len;
        let n = s.len().min(room);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let mut msg = AbortMessage { buf: [0; 192], len: 0 };
    let _ = write!(msg, "{info}");
    unsafe { esp_system_abort(msg.buf.as_ptr().cast()) }
}
