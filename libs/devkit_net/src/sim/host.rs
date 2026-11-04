/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::{CStr, c_int};

#[allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals, unsafe_op_in_unsafe_fn)]
mod ffi {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

pub(super) type Fd = c_int;

pub(super) fn env_fd(name: &CStr) -> Option<Fd> {
    let value = unsafe { ffi::getenv(name.as_ptr()) };
    if value.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(value) }.to_str().ok()?.parse().ok()
}

/// One datagram; `false` when it does not fit in the socket now.
pub(super) fn send(fd: Fd, data: &[u8]) -> bool {
    unsafe { ffi::send(fd, data.as_ptr().cast(), data.len(), ffi::MSG_DONTWAIT as c_int) >= 0 }
}

/// Blocks for one datagram; `None` once the socket fails.
pub(super) fn recv(fd: Fd, buf: &mut [u8]) -> Option<usize> {
    let n = unsafe { ffi::recv(fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
    (n >= 0).then_some(n as usize)
}
