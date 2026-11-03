/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::ffi::{CStr, c_void};
use core::mem;
use core::ptr;
use core::time::Duration;

use crate::{Error, Result, sys};

type Callback = Box<dyn Fn() + Send + Sync>;

/// An `esp_timer`; the callback runs on the esp_timer task.
pub struct Timer {
    handle: sys::esp_timer_handle_t,
    _callback: Box<Callback>,
}

unsafe impl Send for Timer {}
unsafe impl Sync for Timer {}

unsafe extern "C" fn trampoline(arg: *mut c_void) {
    let callback = unsafe { &*arg.cast::<Callback>() };
    callback();
}

impl Timer {
    pub fn new(name: &'static CStr, callback: impl Fn() + Send + Sync + 'static) -> Result<Self> {
        let callback: Box<Callback> = Box::new(Box::new(callback));
        let mut args: sys::esp_timer_create_args_t = unsafe { mem::zeroed() };
        args.callback = Some(trampoline);
        args.arg = ptr::from_ref(&*callback).cast_mut().cast();
        args.dispatch_method = sys::esp_timer_dispatch_t_ESP_TIMER_TASK;
        args.name = name.as_ptr();
        let mut handle = ptr::null_mut();
        Error::check(unsafe { sys::esp_timer_create(&args, &mut handle) })?;
        Ok(Self { handle, _callback: callback })
    }

    /// `INVALID_STATE` while it is running.
    pub fn start_once(&self, after: Duration) -> Result<()> {
        let us = u64::try_from(after.as_micros()).unwrap_or(u64::MAX);
        Error::check(unsafe { sys::esp_timer_start_once(self.handle, us) })
    }

    /// `INVALID_STATE` when it is not running.
    pub fn stop(&self) -> Result<()> {
        Error::check(unsafe { sys::esp_timer_stop(self.handle) })
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        unsafe {
            sys::esp_timer_stop(self.handle);
            sys::esp_timer_delete(self.handle);
        }
    }
}
