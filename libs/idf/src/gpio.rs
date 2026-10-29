/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::c_void;

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

pub fn set_direction(pin: i32, mode: Mode) -> Result<()> {
    let mode = match mode {
        Mode::Disable => sys::gpio_mode_t_GPIO_MODE_DISABLE,
        Mode::Input => sys::gpio_mode_t_GPIO_MODE_INPUT,
        Mode::Output => sys::gpio_mode_t_GPIO_MODE_OUTPUT,
    };
    Error::check(unsafe { sys::gpio_set_direction(pin as _, mode) })
}

pub fn set_level(pin: i32, high: bool) -> Result<()> {
    Error::check(unsafe { sys::gpio_set_level(pin as _, high as u32) })
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
    match Error::check(unsafe { sys::gpio_install_isr_service(0) }) {
        Err(Error::INVALID_STATE) => Ok(()),
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
