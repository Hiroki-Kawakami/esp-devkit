/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{Error, Result, sys};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Disable,
    Input,
    Output,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pull {
    None,
    Up,
    Down,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Interrupt {
    Disable,
    RisingEdge,
    FallingEdge,
    AnyEdge,
    LowLevel,
    HighLevel,
}

fn raw_mode(mode: Mode) -> sys::gpio_mode_t {
    match mode {
        Mode::Disable => sys::gpio_mode_t_GPIO_MODE_DISABLE,
        Mode::Input => sys::gpio_mode_t_GPIO_MODE_INPUT,
        Mode::Output => sys::gpio_mode_t_GPIO_MODE_OUTPUT,
    }
}

pub fn set_direction(pin: i32, mode: Mode) -> Result<()> {
    Error::check(unsafe { sys::gpio_set_direction(pin as _, raw_mode(mode)) })
}

/// Routes each pin to plain GPIO in `mode`, with pulls and interrupts off.
pub fn configure(pins: &[i32], mode: Mode) -> Result<()> {
    let mut cfg: sys::gpio_config_t = unsafe { core::mem::zeroed() };
    cfg.pin_bit_mask = pins.iter().fold(0u64, |mask, &pin| mask | 1 << pin);
    cfg.mode = raw_mode(mode);
    Error::check(unsafe { sys::gpio_config(&cfg) })
}

pub fn set_level(pin: i32, high: bool) -> Result<()> {
    Error::check(unsafe { sys::gpio_set_level(pin as _, high as u32) })
}

/// `set_level` without its checks, a register write: for bit-banging a pin
/// already an output, whose `pin` is never out of range.
#[cfg(target_os = "none")]
pub fn set_level_fast(pin: i32, high: bool) {
    unsafe { sys::idf_gpio_set_level_fast(pin as u32, u32::from(high)) }
}

/// The output levels of a bank of 32 pins, a store setting or clearing any
/// of them together: for bit-banging pins already outputs.
#[cfg(target_os = "none")]
#[derive(Clone, Copy)]
pub struct OutputBank {
    set: *mut u32,
    clear: *mut u32,
}

#[cfg(target_os = "none")]
unsafe impl Send for OutputBank {}
#[cfg(target_os = "none")]
unsafe impl Sync for OutputBank {}

#[cfg(target_os = "none")]
impl OutputBank {
    /// The bank holding `pin`, and the bit of `pin` in it.
    pub fn of(pin: i32) -> (Self, u32) {
        let (mut set, mut clear) = (core::ptr::null_mut(), core::ptr::null_mut());
        unsafe { sys::idf_gpio_out_regs(pin as u32 / 32, &mut set, &mut clear) };
        (Self { set, clear }, 1 << (pin as u32 % 32))
    }

    /// Drives the pins of `mask` high.
    #[inline(always)]
    pub fn set(&self, mask: u32) {
        unsafe { self.set.write_volatile(mask) }
    }

    /// Drives the pins of `mask` low.
    #[inline(always)]
    pub fn clear(&self, mask: u32) {
        unsafe { self.clear.write_volatile(mask) }
    }
}

/// The input level; low on a pin that is not an input.
pub fn level(pin: i32) -> bool {
    unsafe { sys::gpio_get_level(pin as _) != 0 }
}

pub fn set_pull(pin: i32, pull: Pull) -> Result<()> {
    let pull = match pull {
        Pull::None => sys::gpio_pull_mode_t_GPIO_FLOATING,
        Pull::Up => sys::gpio_pull_mode_t_GPIO_PULLUP_ONLY,
        Pull::Down => sys::gpio_pull_mode_t_GPIO_PULLDOWN_ONLY,
    };
    Error::check(unsafe { sys::gpio_set_pull_mode(pin as _, pull) })
}

pub fn set_interrupt(pin: i32, interrupt: Interrupt) -> Result<()> {
    let ty = match interrupt {
        Interrupt::Disable => sys::gpio_int_type_t_GPIO_INTR_DISABLE,
        Interrupt::RisingEdge => sys::gpio_int_type_t_GPIO_INTR_POSEDGE,
        Interrupt::FallingEdge => sys::gpio_int_type_t_GPIO_INTR_NEGEDGE,
        Interrupt::AnyEdge => sys::gpio_int_type_t_GPIO_INTR_ANYEDGE,
        Interrupt::LowLevel => sys::gpio_int_type_t_GPIO_INTR_LOW_LEVEL,
        Interrupt::HighLevel => sys::gpio_int_type_t_GPIO_INTR_HIGH_LEVEL,
    };
    Error::check(unsafe { sys::gpio_set_intr_type(pin as _, ty) })
}

pub fn install_isr_service() -> Result<()> {
    // IDF logs an error on a repeated install even though it returns INVALID_STATE.
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.load(Ordering::Acquire) {
        return Ok(());
    }
    match Error::check(unsafe { sys::gpio_install_isr_service(0) }) {
        Ok(()) | Err(Error::INVALID_STATE) => {
            INSTALLED.store(true, Ordering::Release);
            Ok(())
        }
        r => r,
    }
}

/// `handler` runs in interrupt context with `arg`, which must stay valid until
/// the handler is removed.
pub unsafe fn add_isr_handler(pin: i32, handler: unsafe extern "C" fn(*mut c_void),
                              arg: *mut c_void) -> Result<()> {
    Error::check(unsafe { sys::gpio_isr_handler_add(pin as _, Some(handler), arg) })?;
    Error::check(unsafe { sys::gpio_intr_enable(pin as _) })
}

pub fn remove_isr_handler(pin: i32) -> Result<()> {
    Error::check(unsafe { sys::gpio_isr_handler_remove(pin as _) })
}
