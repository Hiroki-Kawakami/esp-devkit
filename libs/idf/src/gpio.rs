/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::{Error, Result, sys};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Disable,
    Input,
    Output,
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

pub fn install_isr_service() -> Result<()> {
    match Error::check(unsafe { sys::gpio_install_isr_service(0) }) {
        Err(Error::INVALID_STATE) => Ok(()),
        r => r,
    }
}
