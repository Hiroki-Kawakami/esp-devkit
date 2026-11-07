/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::c_void;
use core::ptr;

use crate::{Error, Result, sys};

devkit::bitflags! {
    pub struct Flags: u32 {
        const LEVEL1 = sys::ESP_INTR_FLAG_LEVEL1;
        const LEVEL2 = sys::ESP_INTR_FLAG_LEVEL2;
        const LEVEL3 = sys::ESP_INTR_FLAG_LEVEL3;
        const LEVEL4 = sys::ESP_INTR_FLAG_LEVEL4;
        const LEVEL5 = sys::ESP_INTR_FLAG_LEVEL5;
        const LEVEL6 = sys::ESP_INTR_FLAG_LEVEL6;
        const NMI = sys::ESP_INTR_FLAG_NMI;
        const SHARED = sys::ESP_INTR_FLAG_SHARED;
        const EDGE = sys::ESP_INTR_FLAG_EDGE;
        const IRAM = sys::ESP_INTR_FLAG_IRAM;
        const INTRDISABLED = sys::ESP_INTR_FLAG_INTRDISABLED;
        const LOWMED = sys::ESP_INTR_FLAG_LOWMED;
        const HIGH = sys::ESP_INTR_FLAG_HIGH;
    }
}

pub type Handler = unsafe extern "C" fn(arg: *mut c_void);

/// An allocated interrupt; dropping it frees it.
pub struct Interrupt {
    handle: sys::intr_handle_t,
}

unsafe impl Send for Interrupt {}
unsafe impl Sync for Interrupt {}

impl Interrupt {
    /// `handler` runs in interrupt context with `arg`, which must stay valid
    /// until the interrupt is freed.
    pub unsafe fn new(source: i32, flags: Flags, handler: Handler, arg: *mut c_void) -> Result<Self> {
        let mut handle = ptr::null_mut();
        Error::check(unsafe { sys::esp_intr_alloc(source, flags.bits() as i32, Some(handler), arg, &mut handle) })?;
        Ok(Self { handle })
    }

    /// As `new`, but on a shared line `handler` only runs while
    /// `status_reg & mask` is non-zero.
    pub unsafe fn with_status(source: i32, flags: Flags, status_reg: u32, mask: u32, handler: Handler, arg: *mut c_void) -> Result<Self> {
        let mut handle = ptr::null_mut();
        Error::check(unsafe {
            sys::esp_intr_alloc_intrstatus(source, flags.bits() as i32, status_reg, mask, Some(handler), arg, &mut handle)
        })?;
        Ok(Self { handle })
    }

    pub fn enable(&self) -> Result<()> {
        Error::check(unsafe { sys::esp_intr_enable(self.handle) })
    }

    pub fn disable(&self) -> Result<()> {
        Error::check(unsafe { sys::esp_intr_disable(self.handle) })
    }
}

impl Drop for Interrupt {
    fn drop(&mut self) {
        unsafe { sys::esp_intr_free(self.handle) };
    }
}
